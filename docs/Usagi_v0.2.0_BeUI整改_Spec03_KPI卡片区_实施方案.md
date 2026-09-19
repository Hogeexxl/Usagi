# Usagi v0.2.0 BeUI 整改实施方案 — Spec03：KPI 卡片区

> 施工分支：`feat/v0.2.0-beui-redesign`  
> 前置条件：**Spec02 / Gate S02 已通过。**  
> 本 Spec 处理 KPI 卡片区及“预估费用完整计价分母”所需的最小 Summary 后端修正。

---

# 1. Spec03 范围

## 1.1 必须完成

1. KPI 卡片统一恢复 BeUI 官方 `TiltCard`。
2. 固定 KPI 内容层尺寸：
   - padding：20px
   - 标题：12px
   - 主数值：28px
   - legend / footer：12px
   - Token / Cache bar：5px
3. 保留最多 4 张卡：
   - 总 Token
   - 缓存命中
   - 会话数量
   - 预估费用
4. 模型筛选激活时继续隐藏“会话数量”。
5. 总 Token bar 修复：
   - Input
   - Output
   - Reasoning ⊂ Output
6. Cache bar 修复：
   - cached
   - remaining
   - 两个 legend 都有 dot
7. Token / Cache hover 使用 BeUI shared Motion token，不得写 `0.18s`。
8. NumberTicker 重新对齐官方 `@beui/number-ticker`。
9. KPI compact NumberTicker 外层补完整原值 accessibility。
10. 费用完整性提示重新对齐官方 Gooey `@beui/popover`。
11. 修复费用 Footer：
   - 分母 = 当前筛选范围内全部 root Session
   - 分子 = 其中费用可完整估算的 root Session
12. Loading Skeleton 继续保持结构化、非 flashy。

## 1.2 明确不做

- 不修改定价公式。
- 不新增费用价格源。
- 不修改 Session Table / Drawer 中的费用展示。
- 不修改 Dashboard Filters。
- 不修改 Charts。
- 不修改 Token 数据采集规则。
- 不把 Reasoning 改成与 Input / Output 平级的第三类 Token。
- 不新造 `MetricCard`、`KpiCard` 等替代 TiltCard 的视觉 primitive。
- 不修改官方 TiltCard 内部 surface / radius / glare / perspective / spring。

---

# 2. BeUI 来源与固定参数

## 2.1 必须对齐的 primitive

| 用途 | BeUI 官方来源 | 固定要求 |
|---|---|---|
| KPI 卡容器 | `@beui/tilt-card` | 官方 Tilt / glare / rounded-2xl / perspective |
| 主数字 / legend 数字 | `@beui/number-ticker` | 官方滚动逻辑 |
| 费用提示 | `@beui/popover` | Gooey Popover |
| 自定义 hover | BeUI `ease.ts` shared token | 禁止 magic number |

## 2.2 TiltCard 固定基线

官方 TiltCard 本身保留：

```text
max = 12
glare = true
perspective = 1000px
rounded-2xl
SPRING_MOUSE
hover-capable gating
reduced-motion gating
glare opacity
```

KPI 调用层允许：

```text
border border-border
bg-card
p-5
h-36
text-foreground
```

注意：

> `border border-border bg-card` 本身是 BeUI 官方 TiltCard Preview 的标准使用方式，不得再把它认定为“必须删除的覆盖”。真正禁止的是继续把 border/color 改成强黑边、纯白硬卡或自定义 surface。

固定 card class：

```text
h-36
border border-border
bg-card
p-5
text-foreground
```

不得改官方：

```text
rounded-2xl
glare
tilt
perspective
SPRING_MOUSE
```

## 2.3 字体层级

统一：

```text
标题：
text-xs
font-medium
text-muted-foreground

主数值：
text-[28px]
font-semibold
leading-8
tracking-tight
text-foreground

legend / footer：
text-xs
leading-4
text-muted-foreground
```

`text-xs` = 12px。

legend 动态 value：

```text
12px
text-foreground
tabular-nums
```

不得再使用：

```text
text-[11px]
```

## 2.4 Bar

统一：

```text
height = 5px
rounded-full
```

Tailwind 可用：

```text
h-[5px]
```

不要使用 6px (`h-1.5`)。

---

# 3. 文件范围

## 3.1 前端主要修改

```text
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/MetricGrid.test.tsx
frontend/src/dashboard/format.ts           # 仅现有 formatter 不足时
frontend/src/data/types.ts

frontend/src/ui/beui/tilt-card.tsx
frontend/src/ui/beui/number-ticker.tsx
frontend/src/ui/beui/popover.tsx
frontend/src/ui/beui/popover-position.ts
```

## 3.2 后端最小修改

当前 Summary 费用完整计价分母语义不正确，需要允许修改：

```text
src/usage/aggregate.rs
```

以及 Summary DTO / HTTP 映射实际所在文件；施工前通过编译器 / 搜索定位：

```text
UsageSummary
SummaryUsageDto 对应 response mapper
cost_incomplete_session_count
session_health
```

只允许围绕 Summary cost-completeness count 修改，不借此重构聚合器。

## 3.3 相关测试

允许修改：

```text
src/usage/aggregate.rs 内 unit tests
tests/** 中直接覆盖 Summary response 的测试
frontend/src/dashboard/MetricGrid.test.tsx
```

## 3.4 禁止扩大

```text
frontend/src/dashboard/session/**
frontend/src/dashboard/charts/**
frontend/src/dashboard/FilterControls.tsx
src/cost/**
src/scanner/**
src/codex/**
```

---

# 4. 实施顺序

# S03-1：同步官方 TiltCard / NumberTicker / Popover

先同步：

```text
@beui/tilt-card
@beui/number-ticker
@beui/popover
```

规则同前两 Spec：

```text
Registry 可安全执行 → Install
否则 → 官方 Manual 完整源码同步
```

只做当前仓库 import path 机械适配。

官方 TiltCard 必须核对：

```text
SPRING_MOUSE
useHoverCapable
useReducedMotion
perspective(1000px)
max default 12
glare default true
rounded-2xl
```

官方 NumberTicker：

- 数字滚动 DOM 不简化；
- blur / stagger / reduced-motion 原样；
- formatter API 原样。

官方 Popover：

- Gooey geometry；
- SVG goo filter；
- portal；
- panel；
- open/close spring；
- Escape / outside；
- reduced-motion；

原样保留。

---

# S03-2：统一 KPI constants

`MetricGrid.tsx` 改为：

```ts
const CARD =
  "h-36 border border-border bg-card p-5 text-foreground";

const TITLE =
  "text-xs font-medium leading-4 text-muted-foreground";

const VALUE =
  "mt-1 text-[28px] font-semibold leading-8 tracking-tight text-foreground";

const LEGEND =
  "text-xs leading-4 text-muted-foreground";
```

删除：

```text
text-[11px]
h-1.5
```

bar 改：

```text
h-[5px]
```

KPI Grid 保留：

```text
desktop:
minmax(488px,1fr) 236px 236px 236px
gap-4
```

模型筛选激活时：

```text
Session Count hidden
其余普通卡不自动拉宽
```

---

# S03-3：NumberTicker wrapper 与 accessibility

建立一个**业务 wrapper**可以保留，但只能负责：

- formatter
- visible title
- aria-label
- className

不得复制 NumberTicker 内部实现。

例如：

```tsx
function CompactTicker({
  value,
  format,
  accessibleLabel,
  className
}) {
  return (
    <span
      title={accessibleLabel}
      aria-label={accessibleLabel}
    >
      <NumberTicker
        value={value}
        blur
        format={format}
        className={className}
      />
    </span>
  );
}
```

关键要求：

### Token

可见：

```text
18.4M
```

accessibility / title：

```text
18,400,000
```

### Cost

可见：

```text
$1.24K
```

accessibility / title：

```text
$1,240.00
```

### Rate

可见：

```text
87.4%
```

accessibility：

```text
87.4%
```

完整值 formatter 必须复用已有格式工具，不复制格式规则。

---

# S03-4：总 Token 卡

结构固定：

```text
总 Token
18.4M

[ Input ][ Output base with Reasoning overlay ]

● 输入 12.2M   ● 输出 6.2M   ● 推理 2.1M
```

## 数学关系

固定：

```text
total = input + output
reasoning <= output
other_output = output - reasoning
```

布局比例：

```text
inputPct     = input / total
outputPct    = output / total
reasoningPct = reasoning / total
```

Reasoning 不是第三个 100% 平级 segment。

### DOM / geometry

Bar：

```text
position: relative
height: 5px
overflow-hidden
rounded-full
bg-muted
```

Input：

```text
left = 0
width = inputPct
bg-accent
```

Output base：

```text
left = inputPct
width = outputPct
bg-violet
```

Reasoning overlay：

```text
right = 0
width = reasoningPct
bg-neon
```

因为 Output 位于 bar 右侧，Reasoning 从整个 bar 右边锚定即位于 Output 的右段。

## Hover

### Input

```text
Input opacity 1
Output opacity 0.3
Reasoning opacity 0.3
```

### Output

整个 Output 必须表现为 violet：

```text
Input dim
Output base opacity 1
Output base z-index > Reasoning
Reasoning 被 Output base 覆盖 / 隐去
```

### Reasoning

```text
Reasoning opacity 1
Input opacity 0.3
Output base opacity 0.3
```

当前“Reasoning hover 时 Output 没有 dim”必须修复。

### Motion

删除：

```ts
{ duration: 0.18 }
```

使用 BeUI shared token，优先：

```text
EASE_OUT
```

如果 opacity / scale 是短过渡，可使用官方已有 token 的明确组合；不得新增 KPI 专属 easing constant。

reduced-motion：

```text
duration 0
scaleY 不放大
```

Bar hover 不得改变 segment width。

---

# S03-5：Token Legend

三个 legend item：

```text
● 输入 value
● 输出 value
● 推理 value
```

统一：

```text
12px
gap-1.5
```

Dot 现有 6px 可暂保留；全站 icon/dot 尺寸最终统一在 Final Gate。此处不扩大设计。

每个 legend item：

- pointer hover
- keyboard focus

都触发同一 focus state。

Reasoning aria-label 必须明确：

```text
推理 <完整值>，包含在输出 Token 中
```

---

# S03-6：缓存命中卡

结构：

```text
缓存命中
87.4%

[ cached ][ remaining ]

● 缓存读取 8.7M   ● 输入 10.0M
```

数据：

```text
cached = usage.cached_tokens
input  = usage.input_tokens

cachedPct =
  input > 0
  ? clamp(cached / input, 0..1)
  : 0
```

Bar：

```text
cached    → bg-accent
remaining → bg-muted
```

删除当前：

```text
bg-muted-foreground/15
```

### Legend

两个都必须有 dot：

```text
缓存读取 → accent dot
输入     → muted / neutral dot
```

输入 dot 使用：

```text
bg-muted-foreground
```

这里只标识“整个 Input 100% 基线”，不引入新颜色。

### Hover

缓存读取：

```text
cached 保持
remaining dim
```

输入：

```text
整条 bar 视觉提升为同一整体强调
cached 仍保留内部比例
```

推荐实现：

- 不改 segment width；
- 只提升整体 bar / remaining opacity；
- 禁止重新把 input hover 解释成第二组数据。

Motion 使用 BeUI shared token。

---

# S03-7：会话数量卡

固定：

```text
会话数量
<session_health.total_sessions>

仅统计主线程会话。
```

主数值使用：

```text
usage.session_health.total_sessions
```

**不使用 `usage.session_count`。**

footer：

```text
mt-auto
12px muted
```

模型筛选激活：

```text
整卡隐藏
```

业务规则不变。

---

# S03-8：先修后端费用完整计价 count

## 当前问题

当前 `UsageSummary`：

```text
session_count
= 有 usage_events 的 healthy root Session 数

incomplete_sessions
= 有 usage event 且至少一个 estimated_cost 为 NULL 的 root Session 数

error_sessions
= quarantined root Session 数

health.total_sessions
= session_count + error_sessions

cost_incomplete_session_count
= incomplete_sessions
```

因此当前前端：

```text
total = session_count
complete = session_count - cost_incomplete_session_count
```

遗漏 error root Session，不符合“当前筛选范围全部 root Session”的 Footer 语义。

## 最小修正

保留现有字段名，避免扩大 API：

```text
cost_incomplete_session_count
```

但将其语义修正为：

```text
费用无法完整计价的 root Session 数
= incomplete_sessions + error_sessions
```

即后端：

```text
cost_incomplete_session_count =
  incomplete_sessions
  + error_sessions
```

使用 checked arithmetic。

`session_health.total_sessions` 保持：

```text
session_count + error_sessions
```

所以前端最终：

```text
cost_total_sessions =
  usage.session_health.total_sessions

cost_complete_sessions =
  cost_total_sessions
  - usage.cost_incomplete_session_count
```

这样分子 / 分母来自同一个 root Session 集合。

## Invariant

必须满足：

```text
0 <= cost_incomplete_session_count <= session_health.total_sessions
```

若违反，后端返回 invariant error，不允许前端 `Math.max()` 静默吞掉。

因此删除当前前端：

```ts
Math.max(0, ...)
```

用明确数据直接计算。

---

# S03-9：预估费用卡

Header：

```text
预估费用                 [!]
```

主数值：

```text
$42.68
```

Footer：

```text
12 / 13 会话完整计价
```

### 正常 complete

```text
无 [!]
```

### partial

```text
显示已知费用合计
[!] 点击 → 有部分费用不完整
```

### unknown

```text
—
[!] 点击 → 当前费用无法完整估算
```

数值本身：

```text
text-foreground
```

不得变红。

---

# S03-10：费用 Popover

使用标准 BeUI Gooey Popover：

```tsx
<Popover side="bottom" align="end">
  <PopoverTrigger>
    <button ...>
      <CircleAlert ... />
    </button>
  </PopoverTrigger>
  <PopoverContent>
    {message}
  </PopoverContent>
</Popover>
```

Popover 必须是官方 Gooey：

- SVG goo
- neck
- panel
- portal
- open / close spring
- reduced-motion

业务层只允许：

```text
side
align
content width / max-width
业务文字
trigger aria-label
```

### Trigger

当前 MU 自制：

```text
h-5 w-5 rounded-full hover:bg-destructive/10
```

删除。

本 Spec 不新造 icon button。

采用最小透明语义 trigger：

```text
inline-flex
items-center
justify-center
text-destructive
outline-none
focus-visible:ring...
```

不加额外圆形 background / hover capsule。

图标使用 `CircleAlert`；具体全站 icon size 在 Final Gate 统一，本 Spec 不扩展。

---

# S03-11：Skeleton

Skeleton 数量：

```text
无 model filter → 4
有 model filter → 3
```

固定：

```text
height 144px
p-5
rounded-2xl
border border-border
bg-card
```

内部：

- title skeleton
- value skeleton
- bar skeleton（需要 bar 的卡）
- 不伪造数值

不新增 Spinner、Shimmer rainbow、复杂 Motion。

如果施工时发现 BeUI 当前有完全对应的 Skeleton primitive，可直接复用；否则现有 token-based skeleton composition 允许保留。

---

# S03-12：清理旧实现

必须清除：

```text
TITLE text-[11px]
LEGEND text-[11px]
bar h-1.5
Token / Cache transition duration: 0.18
Cache remaining bg-muted-foreground/15
Cache Input legend 缺 dot
Reasoning hover 时 Output 不 dim
Cost footer total = usage.session_count
Math.max(0, ...)
Cost trigger rounded-full hover:bg-destructive/10
```

不得清除：

```text
TiltCard 调用层的 border border-border bg-card
```

因为这正是 BeUI 官方 TiltCard Preview 的标准 surface 用法。

---

# 5. 最小测试标准

# T-S03-001：KPI 集合与尺寸

自动：

- 无模型筛选 → 4 cards。
- 有模型筛选 → 3 cards，仅隐藏会话数量。
- 标题存在：
  - 总 Token
  - 缓存命中
  - 会话数量
  - 预估费用

浏览器：

- 标题 12px。
- 主数值 28px。
- footer / legend 12px。
- bar 5px。
- Tilt / glare / surface 与 BeUI 官方 TiltCard 一致。

---

# T-S03-002：Token 语义与 hover

使用：

```text
input = 1500
output = 500
reasoning = 125
total = 2000
```

验证：

- Input 75%
- Output 25%
- Reasoning 6.25% 且位于 Output 右段
- Reasoning 不作为独立第三段相加

浏览器交互只测 3 个状态：

1. Hover Input。
2. Hover Output。
3. Hover Reasoning。

PASS：

- dim / overlay 符合 S03-4。
- width 完全不变。
- 无 `0.18` 自定义 Motion。

---

# T-S03-003：Cache bar

使用：

```text
input = 1500
cached = 600
rate = 40%
```

验证：

- cached 40%
- remaining 60%
- remaining 使用 muted
- `缓存读取` 与 `输入` 都有 dot
- hover cached / input 两种状态可用
- width 不变

---

# T-S03-004：费用完整计价口径

后端最小测试覆盖 3 组即可：

### A：全部费用完整

```text
healthy = 3
incomplete = 0
error = 0

total = 3
cost_incomplete = 0
footer = 3 / 3
```

### B：一个费用不完整

```text
healthy = 3
incomplete = 1
error = 0

total = 3
cost_incomplete = 1
footer = 2 / 3
```

### C：一个费用不完整 + 一个 error root

```text
healthy = 3
incomplete = 1
error = 1

health.total_sessions = 4
cost_incomplete = 2
footer = 2 / 4
```

不为更多排列组合建立大量测试。

---

# T-S03-005：Cost Popover

前端：

partial：

```text
数值保留
[!] 可点击
内容 = 有部分费用不完整
```

unknown：

```text
数值 = —
[!] 可点击
内容 = 当前费用无法完整估算
```

complete：

```text
无 [!]
```

浏览器：

- Gooey panel 可见。
- Escape 可关闭。
- Popover 不是纯文字浮层或自制 bubble。

---

# T-S03-006：NumberTicker accessibility

至少验证：

```text
compact token 可见值
完整 token aria-label/title

compact cost 可见值
完整 cost aria-label/title
```

不要求对 NumberTicker 每个 digit DOM 写 snapshot。

---

# 6. 必跑命令

## Frontend

```bash
cd frontend
npm run build
npm test -- src/dashboard/MetricGrid.test.tsx
```

若官方同步涉及 NumberTicker / Popover / TiltCard 已有测试，一并运行对应测试。

## Rust

从仓库根目录：

```bash
cargo fmt --check
cargo test <实际 Summary aggregate 相关测试过滤器>
```

本 Spec 不要求为了一个 Summary 字段修正运行所有长期 stress / scanner tests。

但若无法用稳定过滤器只运行 Summary 聚合测试，则运行：

```bash
cargo test
```

不得跳过失败项。

---

# 7. Gate S03

## Gate S03-A：官方组件

- TiltCard 与当前 BeUI 官方源码一致。
- NumberTicker 与当前 BeUI 官方源码一致。
- Popover 与当前 BeUI 官方 Gooey Popover 一致。
- 无仿写 primitive。

## Gate S03-B：视觉层级

固定：

```text
padding 20px
title 12px
value 28px
legend/footer 12px
bar 5px
```

TiltCard 官方：

```text
rounded-2xl
border-border + bg-card 使用方式
tilt
glare
SPRING_MOUSE
reduced-motion
```

均保留。

## Gate S03-C：Token / Cache

- Reasoning ⊂ Output。
- 三种 Token hover 正确。
- Cache remaining = muted。
- Cache 两个 legend 都有 dot。
- 无 `duration: 0.18`。

## Gate S03-D：费用

- Footer denominator = `session_health.total_sessions`。
- `cost_incomplete_session_count` 包含费用不完整 root + error root。
- partial / unknown 文案正确。
- 数值不变红。
- Gooey Popover 官方化。

## Gate S03-E：Accessibility

- Compact NumberTicker 有完整原值 accessible label/title。
- Reasoning accessible name 明确“包含在输出 Token 中”。
- Popover Trigger 有 aria-label。

## Gate S03-F：工程

```text
frontend npm run build = PASS
MetricGrid targeted tests = PASS
cargo fmt --check = PASS
Summary aggregate targeted tests = PASS
```

全部 PASS 才允许进入 Spec04。

---

# 8. 施工员禁止事项

1. 禁止重写 TiltCard 视觉，只允许官方源码 + 本文批准的内容层参数。
2. 禁止把 BeUI 官方 Preview 使用的 `border-border bg-card` 当成错误删除。
3. 禁止新增 `KpiCard` / `MetricCard` 来包一层再重新定义 surface。
4. 禁止 Reasoning 变成 Input / Output 平级第三段。
5. 禁止改变 bar width 来做 hover。
6. 禁止使用 `0.18` 等 KPI 独立 magic motion。
7. 禁止修改 NumberTicker 内部 DOM 来补 accessibility。
8. 禁止用纯文字 tooltip 替代 Gooey Popover。
9. 禁止为了 Footer 修复重写费用计算公式。
10. 禁止使用 `session_health.complete_sessions` 作为费用完整计价分子。
11. 禁止前端 `Math.max()` 掩盖后端费用 count invariant。
12. 禁止借 Spec03 修改 Table / Drawer / Charts。
13. 禁止过度增加测试；只覆盖本 Spec 的数据语义和关键交互。
14. 禁止以“看起来像 BeUI”作为通过依据。
