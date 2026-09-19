# Usagi v0.2.0 BeUI 整改实施方案 — Spec02：筛选菜单区

> 施工分支：`feat/v0.2.0-beui-redesign`  
> 前置条件：**Spec01 / Gate S01 已通过。**  
> 本 Spec 只处理 Dashboard 筛选菜单区：时间范围、模型筛选、项目筛选、清除筛选及其 Popover 内状态。

---

# 1. Spec02 范围

## 1.1 必须完成

1. 时间范围重新对齐 BeUI 官方 `Tabs`：
   - `variant="pill"`
   - 文案：`今天 / 昨天 / 7d / 30d / 今年`
2. 模型 Trigger、项目 Trigger 使用 Spec01 已官方化的 BeUI `Button`：
   - `size="sm"`
   - 未筛选 `secondary`
   - 已筛选 `primary`
3. 模型 / 项目筛选弹层重新对齐 BeUI 官方 `MorphPopover`。
4. 模型 / 项目 Checkbox 重新对齐 BeUI 官方 `Checkbox`。
5. 普通模型 / 项目必须使用 Checkbox 官方 `label`，点击文字即可切换选择。
6. GPT 父项保留 checked / unchecked / indeterminate 三态。
7. GPT 展开 Chevron 保留 MU 最小 composition，但 Motion 必须使用 BeUI shared motion token。
8. 清除筛选使用 BeUI `Button`：
   - `variant="ghost"`
   - `size="sm"`
9. 保留 loading / stale / error / empty 业务状态，但其 Button 与弹层视觉必须来自标准 BeUI primitive。
10. 保留项目显示 helper：`projectDisplay / projectTitle / projectKey`。

## 1.2 明确不做

- 不修改筛选数据 API。
- 不修改 `useDashboardController` 的筛选业务语义。
- 不修改筛选选项“全历史生成并缓存”的后端逻辑。
- 不修改 KPI。
- 不修改 Table / Drawer / Charts。
- 不新造 `FilterTabs`、`FilterButton`、`FilterCheckbox`、`FilterPopover` 等视觉 primitive。
- 不给官方 Tabs / Button / Checkbox / MorphPopover 再覆盖 font-size、padding、radius、hover、press、spring。

---

# 2. BeUI 组件来源与固定参数

## 2.1 同步规则

延续 Spec01：

```text
官方 Registry Install
> 官方 Manual 完整源码同步
> 最小 import / MU business adaptation
> 禁止“看起来像标准组件”的近似实现
```

当前仓库若仍没有可直接安全执行 Registry Install 的 shadcn 配置，则使用 BeUI 当前官方 Manual 源码完整同步；不得为了本 Spec 运行 `shadcn init` 并重写 Theme。

## 2.2 必须恢复的 primitive

| 用途 | BeUI 官方来源 | 固定参数 / 规则 |
|---|---|---|
| 时间范围 | `@beui/tabs` | `variant="pill"` |
| 模型 Trigger | Spec01 官方 Button | `secondary/primary` + `sm` |
| 项目 Trigger | Spec01 官方 Button | `secondary/primary` + `sm` |
| 模型 Popover | `@beui/popover-morph` | `side="bottom"` / `align="start"` |
| 项目 Popover | `@beui/popover-morph` | `side="bottom"` / `align="start"` |
| 模型 Checkbox | `@beui/checkbox` | checked / unchecked / indeterminate |
| 项目 Checkbox | `@beui/checkbox` | checked / unchecked |
| 清除筛选 | Spec01 官方 Button | `ghost / sm` |
| 重试 | Spec01 官方 Button | `ghost / sm` |

## 2.3 BeUI Tabs 固定视觉基线

业务层只写：

```tsx
<Tabs value={value} onValueChange={...} variant="pill">
  <TabsList>
    ...
  </TabsList>
</Tabs>
```

不得覆写官方 Pill 的：

```text
TabsList:
inline-flex
gap-1
rounded-full
bg-card
p-1

TabsTrigger:
px-3.5
py-1.5
text-sm
font-medium
rounded-full
```

不得修改官方 active `layoutId` indicator spring。

## 2.4 BeUI MorphPopover 固定边界

允许业务层只指定内容尺寸：

```text
模型：w-72
项目：w-80
```

以及必要的内容 padding：

```text
p-2
```

这些属于 `MorphPopoverContent className` 的内容布局，不允许改官方：

- `border`
- `bg-background`
- radius
- drop-shadow
- clip morph
- portal
- positioning
- open / close Motion
- reduced-motion

## 2.5 BeUI Checkbox 固定边界

普通 Checkbox 必须使用官方：

```tsx
<Checkbox
  checked={...}
  onCheckedChange={...}
  label="..."
/>
```

业务层不得重写：

- 20×20 box
- `rounded-md`
- border-2
- checked fill
- mark draw
- press scale
- indeterminate line
- focus ring
- reduced-motion

---

# 3. 文件范围

## 3.1 主要修改

```text
frontend/src/dashboard/RangeSelector.tsx
frontend/src/dashboard/RangeSelector.test.tsx

frontend/src/dashboard/FilterControls.tsx
frontend/src/dashboard/FilterControls.test.tsx   # 若当前不存在则允许新增

frontend/src/ui/beui/tabs.tsx
frontend/src/ui/beui/checkbox.tsx
frontend/src/ui/beui/morph-popover.tsx
frontend/src/ui/beui/popover-position.ts
```

## 3.2 只读依赖

```text
frontend/src/dashboard/DashboardPage.tsx
frontend/src/dashboard/useDashboardController.ts
frontend/src/dashboard/shared/projectDisplay.ts
frontend/src/data/types.ts

frontend/src/ui/beui/button/**
frontend/src/ui/lib/ease.ts
frontend/src/ui/lib/cn.ts
```

Spec01 已通过后，Button 不得在 Spec02 再次修改视觉实现。

## 3.3 禁止扩大

```text
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/session/**
frontend/src/dashboard/charts/**
frontend/src/theme/**
src/**
```

---

# 4. 实施顺序

# S02-1：同步官方 Tabs / Checkbox / MorphPopover

先同步 primitive，再修改业务调用层。

必须同步：

```text
@beui/tabs
@beui/checkbox
@beui/popover-morph
```

同时把官方依赖的：

```text
popover-position
shared ease token
cn / utils
```

按仓库现有路径做机械 import 适配。

禁止保留当前 MU primitive 中与官方不同的：

- 自定义 trigger 注册；
- root / anchor fallback；
- 自定义 close；
- 自定义 focus restore；
- 自定义 panel surface；
- 自定义 checkbox mark / animation。

如果官方源码本身已经包含某行为，以官方行为为准。

---

# S02-2：时间范围

`RangeSelector.tsx` 保持业务数据：

```ts
today      → 今天
yesterday  → 昨天
7d         → 7d
30d        → 30d
year       → 今年
```

调用固定：

```tsx
<Tabs
  value={value}
  onValueChange={(range) => onChange(range as RangeKey)}
  variant="pill"
>
```

删除不必要的：

```text
className="range-selector"
```

若该 class 没有纯布局用途，直接删除对应 CSS。

不得给：

```text
TabsList
TabsTrigger
```

传视觉 className。

---

# S02-3：模型 / 项目 Trigger

保留 `FilterTrigger` 作为**业务 composition**，但它只能组合官方 Button，不得自带视觉系统。

固定：

```tsx
<Button
  variant={count > 0 ? "primary" : "secondary"}
  size="sm"
  ...
>
```

文案：

```text
模型 · 全部
模型 · N 项

项目 · 全部
项目 · N 项
```

aria-label：

```text
模型筛选，全部
模型筛选，已选N项
项目筛选，全部
项目筛选，已选N项
```

图标：

```text
模型 → Lucide Cpu
项目 → Lucide Folder
```

图标只作为 Button 内容，不覆盖 Button 内部 padding / gap。

图标尺寸暂按现有 16px 保留；全站 icon sizing 在最终 Gate 再统一，不在本 Spec 扩大。

---

# S02-4：模型 MorphPopover

固定：

```tsx
<MorphPopover>
  <MorphPopoverTrigger>
    <FilterTrigger ... />
  </MorphPopoverTrigger>

  <MorphPopoverContent
    side="bottom"
    align="start"
    className="w-72 p-2"
  >
    ...
  </MorphPopoverContent>
</MorphPopover>
```

Popover 内部按以下顺序：

```text
OptionStatus（如有）
GPT 组（如有）
其他模型
```

空状态：

```text
暂无模型
```

不得新增：

- Search input
- Apply / Cancel
- Footer
- 自制 close button

当前筛选逻辑继续即时生效。

---

# S02-5：普通模型 Checkbox 改为官方 label

当前：

```text
Checkbox + 独立 span
```

改为：

```tsx
<Checkbox
  checked={selectedModels.has(model)}
  onCheckedChange={() => toggleModel(model)}
  label={model}
/>
```

要求：

- 点击方框切换。
- 点击模型文字切换。
- 长模型名仍需要避免撑破 panel。

因为官方 `label` 本身是 `span`，若长文本需要 truncate，只允许在**调用层外部的宽度约束**或官方公开 `className` 可表达的范围内处理；不得修改 Checkbox 内部 DOM。

如果官方 `label: string` API 无法单独给 label span 加 truncate class，本 Spec 允许做的顺序固定为：

1. 先测试 `w-72` 下实际模型名是否会破版；
2. 不破版 → 不修改；
3. 确实破版 → 只允许给官方 Checkbox 增加一个最小 `labelClassName?: string` 扩展；
4. 该扩展只能透传到官方 label `<span>` 的 `className`；
5. 不得改 Checkbox 其它 DOM / Motion。

这是 Spec02 唯一预批准的 Checkbox 源码扩展；没有实际破版不得添加。

---

# S02-6：GPT 父项 tri-state

业务计算保持：

```text
none selected → unchecked
some selected → indeterminate
all selected  → checked
```

Checkbox：

```tsx
<Checkbox
  checked={allGptSelected}
  indeterminate={someGptSelected && !allGptSelected}
  aria-label="GPT"
  onCheckedChange={toggleGpt}
/>
```

GPT 父项不使用整行 Checkbox `label`，因为同一行存在两个动作：

```text
Checkbox → 全组选中 / 取消
GPT文字 + Chevron → 展开 / 收起
```

结构允许：

```text
[Checkbox] [GPT ---------------- Chevron]
```

但：

- Checkbox 必须官方；
- GPT 展开按钮不得套第二个视觉 Button；
- 不增加 background / border / capsule。

---

# S02-7：GPT Chevron Motion

当前硬编码：

```ts
{ duration: 0.18 }
```

删除。

优先使用 Spec01 / BeUI 官方 `ease.ts` 中的 shared token。

推荐：

```tsx
transition={reduce ? { duration: 0 } : SPRING_LAYOUT}
```

如果当前官方 `ease.ts` 没有 `SPRING_LAYOUT`，则使用官方当前源码提供的等价 layout / swap token；禁止新造：

```text
FILTER_CHEVRON_TRANSITION
0.18
0.2
自定义 cubic-bezier
```

状态：

```text
expanded  → rotate 90deg
collapsed → rotate 0deg
```

reduced-motion：

```text
duration 0
```

---

# S02-8：项目 MorphPopover + Checkbox label

固定：

```tsx
<MorphPopoverContent
  side="bottom"
  align="start"
  className="w-80 p-2"
>
```

项目每项使用官方 Checkbox `label`。

显示：

```text
projectDisplay(project)
```

辅助完整信息继续：

```text
projectTitle(project)
```

项目选择 payload 继续：

```text
project → project_path
projectless → kind
unknown → kind
```

不得把 `project_name` 当后端筛选 key。

若官方 Checkbox `label` 已足够承载文本，则删除当前独立 `<span title=...>`。

对完整 path 的提示继续使用 native `title` 即可；本 Spec 不引入 Tooltip。

---

# S02-9：OptionStatus

保留四种业务状态：

```text
loading
stale
error
empty
```

固定文案：

```text
loading → 选项加载中…
error   → 选项加载失败
stale + 有缓存 → 选项可能已更新
stale + 无缓存 → 选项需要刷新
empty model → 暂无模型
empty project → 暂无项目
```

Retry：

```tsx
<Button variant="ghost" size="sm">
  重试
</Button>
```

状态文字：

```text
loading / empty → muted
error           → destructive
stale           → warning
```

不造 `StatusBanner` 视觉 primitive。

---

# S02-10：清除筛选

显示条件：

```text
anyFilterActive === true
```

固定：

```tsx
<Button
  variant="ghost"
  size="sm"
  onClick={onClear}
>
  清除筛选
</Button>
```

禁止 destructive。

点击后：

```text
models = []
projects = []
```

时间范围不清空、不改变。

---

# S02-11：清理旧实现

必须清除：

```text
当前非官方 tabs.tsx 差异实现
当前非官方 checkbox.tsx 差异实现
当前 MorphPopover fork 差异逻辑
普通模型：Checkbox + 独立不可点击 span
普通项目：Checkbox + 独立不可点击 span
GPT Chevron duration: 0.18
rowClass 中 hover:bg-primary/5 的自制 hover row
```

对于普通行，不再维护当前：

```text
rounded-xl
hover:bg-primary/5
```

这种自制 selectable row。

普通项的点击反馈以官方 Checkbox + label 为主。

GPT 父项如果需要整行对齐，仅保留布局：

```text
flex
items-center
gap
```

不得额外制造自定义 hover surface。

---

# 5. 最小测试标准

# T-S02-001：时间 Tabs

自动测试：

- 5 个标签存在。
- 当前 `value` 对应 `aria-selected=true`。
- 点击另一个标签调用 `onChange` 正确 RangeKey。

浏览器验收：

- 标准 BeUI Pill 背景 / active indicator / hover / spring 可见。
- 没有裸文字按钮视觉。
- 不存在业务层 TabsTrigger 尺寸覆盖。

**PASS：业务切换正确 + 实际视觉与 BeUI Tabs Pill 一致。**

---

# T-S02-002：模型筛选

至少覆盖：

1. 无选择 → Trigger `secondary` 语义，显示 `模型 · 全部`。
2. 选择 1 个 → Trigger `primary` 语义，显示 `模型 · 1 项`。
3. 点击普通模型 checkbox → 选中。
4. 点击普通模型文字 → 同样切换。
5. GPT：
   - 0/N = unchecked
   - 部分 = indeterminate
   - N/N = checked
6. GPT 展开 / 收起正常。

浏览器复验：

- Checkbox 外观 / mark draw / press / indeterminate 为官方效果。
- Chevron 没有独立 magic-number 动画。

---

# T-S02-003：项目筛选

至少覆盖：

- 正常项目点击文字可选择。
- `projectless` 可选择。
- `unknown` 可选择。
- normal project 提交 `project_path`，不是 `project_name`。
- Trigger count 正确。

浏览器复验：

- `w-80` panel 不破版。
- Checkbox / MorphPopover 为官方视觉。

---

# T-S02-004：MorphPopover 状态

对模型或项目任一弹层验证：

- Trigger click 打开。
- Escape 关闭。
- Outside pointer 关闭。
- panel 从 trigger 角 clip-morph 展开。
- panel border/background/drop-shadow 为 BeUI 官方表现。
- reduced-motion 下无非必要 Motion。

不为模型和项目分别写重复测试。

---

# T-S02-005：清除与错误状态

自动测试：

- 有筛选时显示 `清除筛选`。
- 无筛选时不显示。
- 点击只清 models/projects，不改 Range。
- error / stale 状态显示 `重试` 且回调正确。

**PASS：不新增额外状态控件。**

---

# 6. 必跑命令

在 `frontend/`：

```bash
npm run build
```

必须 PASS。

相关 Vitest：

```bash
npm test -- \
  src/dashboard/RangeSelector.test.tsx \
  src/dashboard/FilterControls.test.tsx
```

若 BeUI primitive 已有对应单测并被本轮同步修改，则把以下相关测试加入：

```text
tabs
checkbox
morph-popover
```

不要求跑 KPI / Table / Drawer / Charts 专项 Gate。

---

# 7. Gate S02

## Gate S02-A：官方 primitive

- `Tabs` = 当前官方 BeUI Tabs。
- `Checkbox` = 当前官方 BeUI Checkbox。
- `MorphPopover` = 当前官方 BeUI MorphPopover。
- Button 沿用 Gate S01 已通过版本。
- 无“看起来像官方”的替代 primitive。

## Gate S02-B：时间范围

```text
今天 / 昨天 / 7d / 30d / 今年
```

- `variant="pill"`
- 官方 active indicator
- 官方 typography
- 官方 hover / spring
- 无业务层视觉覆盖

## Gate S02-C：模型

- Trigger `secondary/primary + sm`
- 普通文字可点击选择
- GPT tri-state 正确
- GPT Chevron Motion 使用 BeUI token
- 无 `0.18` magic number

## Gate S02-D：项目

- Trigger `secondary/primary + sm`
- 普通文字可点击选择
- `project_path` 仍为筛选 key
- projectless / unknown 保持可用

## Gate S02-E：Popover / Clear

- 模型 / 项目使用标准 MorphPopover
- Escape / outside close 正常
- 清除筛选 = `ghost / sm`
- loading / stale / error / empty 业务状态正常

## Gate S02-F：工程

```text
npm run build = PASS
Spec02 targeted tests = PASS
```

任何一项 FAIL，Spec02 不通过，不进入 Spec03。

---

# 8. 施工员禁止事项

1. 禁止在 `RangeSelector.tsx` 给 `TabsList / TabsTrigger` 重新写尺寸。
2. 禁止给 Filter Trigger 手写圆角、padding、hover、press。
3. 禁止继续维护当前 MorphPopover fork。
4. 禁止继续维护当前 Checkbox fork。
5. 禁止普通项目 / 模型继续使用“Checkbox + 不可点击 span”。
6. 禁止把 GPT 父项整行变成一个 Checkbox，破坏“选择”和“展开”两个独立动作。
7. 禁止引入 Accordion 只为 GPT 子组展开。
8. 禁止写 `duration: 0.18` 等筛选区专属动画参数。
9. 禁止添加搜索框、Apply、Cancel、Footer 等未批准功能。
10. 禁止借 Spec02 修改 KPI / Table / Drawer / Charts。
11. 禁止以“视觉差不多”判通过；必须核验官方源码来源与实际页面效果。
