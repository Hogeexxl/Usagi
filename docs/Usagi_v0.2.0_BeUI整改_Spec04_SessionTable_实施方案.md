# Usagi v0.2.0 BeUI 整改实施方案 — Spec04：Session Table

> 施工分支：`feat/v0.2.0-beui-redesign`  
> 前置条件：**Spec03 / Gate S03 已通过。**  
> 本 Spec 只处理 Session 记录列表、Table、排序表头、分页、页码 Input、行交互与 Table loading/empty。Drawer 不在本 Spec。

---

# 1. Spec04 范围

## 1.1 必须完成

1. `@beui/table` 整套源码重新以 BeUI 当前官方版本为基线同步。
2. Table 官方源码同步后，只重新施加 **2 个已批准的 MU 扩展**：
   - `manualSort`
   - `getRowProps`
3. Table surface 恢复官方：
   - `bg-background`
   - `border border-border`
   - 官方 muted header
   - 官方 row separators
   - 官方 `hover:bg-muted/50`
4. 删除当前业务层 `bg-card` 覆盖。
5. 8 列顺序、sortable 规则按本文固定。
6. 列宽重新按本文固定，正常桌面不出现横向滚动条。
7. 数字列右对齐并使用 `tabular-nums`。
8. Table rowHeight 固定 48px。
9. Ready 状态 Table 高度改为根据当前实际行数动态计算，最多 15 行。
10. Loading 使用 BeUI 官方 `SkeletonRows`。
11. Empty 文案固定：
   - `当前时间范围暂无 Session 记录`
12. 保留现有“全量轻量 sort_index → 全局排序 → 60-row window → 15-row page”架构。
13. 保留第 3 页预取下一 60-row window。
14. 排序 Header 必须使用 BeUI 官方 TableHeader，业务层不得重做 Chevron / hover / active Motion。
15. 可用行支持：
   - click
   - Enter
   - Space
   打开 Drawer。
16. error 行：
   - 不可 click
   - 不可 keyboard activate
   - `aria-disabled=true`
17. incomplete/error 状态只在“标题”列左侧显示语义图标，并使用标准 BeUI Tooltip。
18. 分页保留在“Session 记录”同一行右侧。
19. 分页按钮继续使用 BeUI `Button secondary / sm`。
20. 页码 Input 重新同步为 BeUI 官方 Input，并通过官方 `classNames` slot 做 compact 适配。
21. 修复真实已复现 bug：
   - 当前第 2 页
   - 输入 3
   - Enter
   - 必须进入第 3 页并保持 Input=3。

## 1.2 明确不做

- 不修改 Drawer。
- 不修改 Session 数据 API。
- 不修改后端排序数据结构。
- 不把分页改成 infinite scroll。
- 不使用 BeUI `onEndReached`。
- 不开启 Table row selection。
- 不开启 column resize。
- 不开启 column reorder。
- 不开启 cell edit。
- 不增加 checkbox 列。
- 不增加行内操作菜单。
- 不为 error 行增加另一套 Card / Banner。
- 不自制 Table Header。
- 不用 CSS `<colgroup>` 覆盖 BeUI Table；只使用 `TableColumn.width`。

---

# 2. BeUI 组件来源与固定规则

## 2.1 Table 同步规则

必须同步**当前完整官方 `@beui/table` 文件集**，不是只替换一个 `index.tsx`。

至少包括官方当前版本所需的：

```text
table/index.tsx
table/types.ts
table/table-header.tsx
table/skeleton-rows.tsx
table/editable-cell.tsx
table/row-handle.tsx
table/table-menu.tsx
table/use-column-sort.ts
table/use-column-resize.ts
table/use-column-reorder.ts
table/use-row-selection.ts
table/utils.ts
```

以及官方依赖的当前 BeUI utility。

同步后再重新施加本文批准的 2 个扩展。

### 注意

当前 BeUI 官方 Table 已经包含：

```text
useRootFontSize
resolveColumnWidth
minTableWidth
fixed table layout
absolute width floor
```

这些属于**当前官方源码**，不是 MU 私自扩展，不得删除。

## 2.2 只批准两个 Table 源码扩展

### 扩展 A：`manualSort`

`TableProps<T>` 增加：

```ts
manualSort?: boolean;
```

作用：

> 保留 BeUI Table Header 的 sortable UI、sort state、Chevron、onSortChange，但禁止 BeUI 在当前 15 行 `data` 内再次做本地排序。

实现范围固定：

1. `Table` props 接受 `manualSort=false`
2. 传入 `useColumnSort`
3. `useColumnSort` 的 `sortedRows`：

```ts
if (manualSort || !sort) return rows;
```

除此之外不得修改官方 sort hook。

### 扩展 B：`getRowProps`

`TableProps<T>` 增加：

```ts
getRowProps?: (
  row: T,
  id: string
) => HTMLAttributes<HTMLTableRowElement>;
```

作用：

> 给 MU Session row 注入 click / keyboard / aria，不 fork BeUI row renderer。

实现必须：

- 保留官方 `<tr>` DOM；
- `className` 通过 `cn(officialClasses, injected.className)` merge；
- `style` 通过 `{ height: rowHeight, ...injected.style }` merge；
- 没有 row menu 时允许 injected pointer handlers 生效；
- 不删除官方 hover / border / selected classes。

### 禁止第三个扩展

不得新增：

```text
rowClassName
cellClassName
headerClassName
disableVirtualization
pageSize
autoHeight
compact
variant="mu"
```

本 Spec 需要的其它行为全部在业务调用层完成。

---

# 3. 官方 Table 视觉基线

必须保持官方：

```text
root:
w-full
overflow-hidden
border border-border
bg-background
text-sm
```

scroll viewport：

```text
overflow-auto
height = Table.height prop
```

table：

```text
border-collapse
table-layout: fixed
min-width official logic
```

header：

```text
sticky top-0
bg-muted
border-b border-border
font-medium
text-muted-foreground
```

普通 row：

```text
border-b border-border/60
transition-colors
hover:bg-muted/50
```

cell：

```text
truncate
px-4
text-foreground
```

不得给 Table 再套：

```text
bg-card
glass
strong black border
custom row hover
scale
tilt
ripple
left active line
```

Table 外观唯一业务级 class：

```text
rounded-2xl
```

---

# 4. 文件范围

## 4.1 主要修改

```text
frontend/src/dashboard/session/SessionTable.tsx
frontend/src/dashboard/session/SessionTableFooter.tsx
frontend/src/dashboard/session/SessionSection.tsx
frontend/src/dashboard/session/useSessionTableController.ts   # 原则上只修分页/Input联动测试需要时
frontend/src/dashboard/session/*.test.tsx / *.test.ts

frontend/src/ui/beui/input.tsx
frontend/src/ui/beui/table.tsx
frontend/src/ui/beui/table/**
frontend/src/ui/beui/tooltip.tsx
```

## 4.2 只读依赖

```text
frontend/src/dashboard/session/sessionFormat.ts
frontend/src/dashboard/session/sessionTypes.ts
frontend/src/dashboard/format.ts
frontend/src/data/types.ts
frontend/src/ui/beui/button/**
frontend/src/ui/lib/ease.ts
frontend/src/ui/lib/cn.ts
```

## 4.3 禁止扩大

```text
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/FilterControls.tsx
frontend/src/dashboard/charts/**
frontend/src/dashboard/session/SessionDetailDrawer.tsx
src/**
```

---

# 5. 实施顺序

# S04-1：先同步官方 Table 全文件集

以 BeUI 当前官方 Table 为基线完整覆盖当前本地 Table 文件集。

完成官方同步后，先确保：

```text
manualSort = 尚未加入
getRowProps = 尚未加入
```

即先得到真正官方 baseline。

然后只执行 S04-2、S04-3 两个扩展。

不得拿当前本地 Table fork 逐行“修到差不多”。

---

# S04-2：重新施加 `manualSort`

按第 2.2 节固定实现。

原因：

当前 MU 已经在 `useSessionTableController` 中完成：

```text
完整 sort_index
→ 全局 comparator
→ sortedIds
→ 当前 60-row window
→ 当前 15-row page
```

如果 BeUI Table 再对 15 rows 本地排序，会破坏全局分页排序。

`manualSort` 只负责跳过：

```text
current data array local sort
```

Table Header 仍然必须：

- clickable；
- active；
- aria-sort；
- Chevron rotate；
- BeUI 官方 Motion；
- `onSortChange` 回调。

---

# S04-3：重新施加 `getRowProps`

按第 2.2 节固定实现。

业务层 `SessionTable.tsx` 用它注入：

```text
data-session-root-id
tabIndex
aria-disabled
aria-selected
onClick
onKeyDown
cursor
```

不修改 BeUI row renderer。

---

# S04-4：同步官方 BeUI Input

当前 `frontend/src/ui/beui/input.tsx` 是简化 native wrapper，废止。

同步 BeUI 当前官方 Input。

必须保留官方：

```text
label
controlled/uncontrolled value
onChange(value: string)
error
success
leftIcon/rightIcon
classNames slots
focus ring
error shake
success icon
reduced-motion
rounded-full field
```

业务层分页只使用它的公开 API。

不得为了分页把 Input primitive 改成 compact 版本。

---

# S04-5：同步官方 BeUI Tooltip

Session Table 状态图标依赖 Tooltip。

重新同步当前官方 Tooltip，保留：

```text
default delay = 120ms
portal
fixed positioning
rounded-lg
border border-border
bg-background
text-xs
shadow-lg
blur + spring
warm window
touch handling
reduced-motion
```

业务层只传：

```text
content
side="top"
```

不得再覆盖 Tooltip surface。

---

# S04-6：固定 8 列与列宽

最终列顺序：

| # | key | 标题 | sortable | width | align |
|---|---|---|---|---:|---|
| 1 | `last_activity` | 最后活动 | 是 | 128px | left |
| 2 | `title` | 标题 | 否 | 不指定 | left |
| 3 | `project` | 项目 | 是 | 150px | left |
| 4 | `model` | 模型 | 是 | 150px | left |
| 5 | `total_tokens` | 总 Token | 是 | 120px | right |
| 6 | `combined_total_tokens` | 合计 Token | 是 | 120px | right |
| 7 | `cache_hit_rate` | 缓存命中率 | 是 | 112px | right |
| 8 | `combined_estimated_cost` | 合计费用 | 是 | 96px | right |

### 当前需要修正

```text
model: 166 → 150
cache_hit_rate: 96 → 112
combined_estimated_cost: 112 → 96
```

### Title

标题列不传 `width`：

```text
吸收剩余宽度
```

长标题：

```text
truncate
```

不得新增自定义 `colgroup` CSS。

### 桌面验收

至少在：

```text
1280px viewport
1440px viewport
```

确认：

- 8 列全部可见；
- 正常页面没有横向滚动条；
- Title 吸收余量。

---

# S04-7：数字列格式

以下列全部：

```text
总 Token
合计 Token
缓存命中率
合计费用
```

外层统一：

```text
tabular-nums
```

仍使用现有 formatter：

```text
formatSessionTokenInteger
formatRatio
formatCost
```

不为 Table 新建另一套 formatter。

`—` 保持原语义。

---

# S04-8：状态图标

仅标题列左侧显示：

### incomplete

```text
CircleAlert
warning semantic
Tooltip = 数据不完整
```

### error

```text
CircleX
destructive semantic
Tooltip = 数据计算异常
```

要求：

- 图标不新增背景圆；
- 图标不独立占一列；
- Tooltip 使用 S04-5 官方版本；
- 具体全站 icon sizing 留 Final Gate，不在本 Spec 重构。

当前 16px 可以暂保留。

---

# S04-9：排序架构保持不变

`useSessionTableController.ts` 保留：

```ts
FRONTEND_PAGE_SIZE = 15;
ROW_BATCH_LIMIT = 60;
```

默认：

```text
last_activity / desc
```

字段第一次切换默认：

```text
project                  asc
model                    asc
last_activity            desc
total_tokens             desc
combined_total_tokens    desc
combined_estimated_cost  desc
cache_hit_rate           desc
```

Comparator：

- text null-last；
- number null-last；
- root_session_id 做稳定 tie-break。

费用排序：

```text
combined_estimated_cost
```

partial-known cost 仍按已知费用值参与排序。

### 不允许改成

```text
后端直接对 15 rows 排序
Table 本地只排当前页
无限滚动
```

---

# S04-10：预取架构保持不变

保留：

```text
4 pages × 15 rows = 60-row window
```

当：

```ts
(page - 1) % 4 === 2
```

即每个 window 的第 3 页时：

```text
后台预取下一 60-row window
```

不需要 Table `onEndReached`。

`onEndReached` 不传。

---

# S04-11：Table 动态高度

固定：

```ts
const TABLE_ROW_HEIGHT = 48;
const TABLE_PAGE_SIZE = 15;
const TABLE_EMPTY_HEIGHT = 192;
const TABLE_INITIAL_LOADING_HEIGHT = 720;
```

Ready 有数据：

```ts
height =
  TABLE_ROW_HEIGHT *
  (Math.min(rows.length, TABLE_PAGE_SIZE) + 1);
```

包含：

```text
1 个 header row
+
实际 data rows
```

例：

```text
15 rows → 768px
10 rows → 528px
3 rows  → 192px
```

### Empty

```text
192px
```

不展示固定 16-row 空壳。

### Initial Loading / page Loading

当：

```text
loading && rows.length === 0
```

使用：

```text
720px
```

让官方 initial loading 分支生成 15 个 `SkeletonRows`。

若已有旧 rows 正在 refresh，不把 Table 突然重置为 720；继续按当前 rows 动态高度，同时使用 `loading` 追加官方 skeleton。

### 禁止

不得为了 auto-height 增加第三个 Table primitive 扩展。

只动态传 `height` prop。

---

# S04-12：Table 调用参数

最终核心调用：

```tsx
<Table
  data={rows}
  columns={columns}
  getRowId={(row) => row.root_session_id}
  rowHeight={48}
  height={tableHeight}
  loading={loading}
  skeletonRows={15}
  emptyState="当前时间范围暂无 Session 记录"
  sort={controlledSort}
  onSortChange={...}
  manualSort
  selectable={false}
  resizable={false}
  reorderable={false}
  className="rounded-2xl"
  getRowProps={...}
/>
```

明确删除：

```text
bg-card
```

不要传：

```text
onEndReached
```

---

# S04-13：行交互

## complete / incomplete

如果有 `onOpenSession`：

```text
tabIndex = 0
cursor-pointer
click → open
Enter → open
Space → preventDefault + open
```

## error

固定：

```text
tabIndex = -1
aria-disabled = true
无 onClick
无 onKeyDown activate
cursor-default
```

error row 仍使用官方 row surface / hover，不建立专属红色整行背景。

### Drawer selected row

可以继续提供：

```text
aria-selected
```

但不增加 MU 自制 selected style。

---

# S04-14：分页布局

保持 `SessionSection`：

```text
左：Session 记录
右：共 N 条  X/Y  [上一页] [下一页] [页码 Input]
```

同一行。

按钮固定：

```text
上一页 → Button secondary / sm
下一页 → Button secondary / sm
重试   → Button ghost / sm
```

不移动到 Table 底部另起一行。

---

# S04-15：分页 Input compact 适配

官方 Input 默认是 44px 高圆形 field，本项目分页场景需要与 BeUI `Button sm` 32px 高对齐。

只允许通过官方公开 `classNames` slot：

```tsx
<Input
  aria-label="跳转页码"
  inputMode="numeric"
  type="text"
  value={value}
  onChange={setValue}
  onKeyDown={...}
  onBlur={...}
  disabled={...}
  className="w-14"
  classNames={{
    field: "h-8",
    input: "px-2 text-center text-xs leading-4 tabular-nums",
  }}
/>
```

说明：

- `className="w-14"` 控制 root width；
- `field: "h-8"` 只做 compact 高度；
- `input` 只做页码排版；
- 官方 rounded-full / border / focus / Motion 保留。

禁止：

```text
修改 Input.tsx 默认高度
新增 size="sm" 到 Input primitive
新增 PaginationInput primitive
重写 focus ring
```

---

# S04-16：修页码 Enter bug

保持本地 draft：

```ts
const [value, setValue] = useState(String(page));

useEffect(() => {
  setValue(String(page));
}, [page]);
```

实现一个单一：

```ts
commitPage()
```

逻辑：

```text
parse value
↓
不是 safe integer
或 <1
或 >totalPages
→ reset 为当前 page

合法且 == 当前 page
→ normalize 为 String(page)

合法且 != 当前 page
→ onGoToPage(target)
→ 此处不要立刻 setValue(String(page))
→ 等父级 page prop 更新后由 useEffect 同步
```

Enter：

```ts
if (event.key === "Enter") {
  event.preventDefault();
  commitPage();
}
```

不得为了 Enter 强制 `blur()`。

Blur：

```text
commitPage()
```

### 必须修复的真实场景

```text
current page = 2
totalPages >= 3
Input 输入 3
按 Enter
```

结果必须：

```text
onGoToPage(3)
parent rerender page=3
Input 保持 "3"
```

不得跳转后闪回 `"2"`。

---

# S04-17：Loading / Empty / Error

### Loading

使用 Table 官方：

```text
SkeletonRows
```

不自制 Table skeleton。

### Empty

精确文案：

```text
当前时间范围暂无 Session 记录
```

### page error

分页区域：

```text
加载页面失败
[重试]
```

### full load error

SessionSection 现有：

```text
Session 记录加载失败
或
Session 记录更新失败
[重试]
```

保留。

不新增 Toast，本 Spec 不改变错误反馈形态。

---

# S04-18：清理旧实现

必须清除：

```text
Table className="rounded-2xl bg-card"
fixed height={48 * 16}
model width 166
cache rate width 96
cost width 112
bare custom input.tsx
pagination Input event-style onChange
pagination Enter 后错误 reset
```

并检查 Table primitive：

只允许出现 MU 扩展：

```text
manualSort
getRowProps
```

不能还有第三个 MU-specific prop。

注意：

以下当前内容如果与官方当前源码一致，必须保留：

```text
useRootFontSize
resolveColumnWidth
minTableWidth
official TableHeader 0.18 Chevron transition
```

其中 `0.18` 是**官方 TableHeader 内部参数**，不是 MU custom Motion，禁止擅自清除。

---

# 6. 最小测试标准

# T-S04-001：Table 官方基线 + 两个扩展

源码核验：

- Table 文件集与当前 BeUI 官方一致；
- diff 只允许：
  - import path mechanical adaptation
  - `manualSort`
  - `getRowProps`

功能：

- `manualSort=true` 时输入 rows 顺序保持；
- 点击 sortable header 仍调用 sort callback；
- `getRowProps` 注入 aria / click 生效。

不为 Table 其它官方功能重复建立测试。

---

# T-S04-002：列、宽度、排序

自动 / DOM 检查：

```text
8 列顺序正确
title 不 sortable
其它 7 列 sortable
数字列 align right
```

浏览器 1280 / 1440：

- 8 列全部可见；
- 没有正常桌面横向滚动；
- Title 吸收剩余宽度；
- Header hover / active / Chevron 与 BeUI 官方一致。

---

# T-S04-003：全局排序 / 分页架构

只验证必要行为：

1. 排序使用完整 `sort_index`，不是当前页 15 rows。
2. `FRONTEND_PAGE_SIZE=15`。
3. `ROW_BATCH_LIMIT=60`。
4. page 3 会触发下一 window prefetch。
5. null-last 保持。

已有 controller tests 可覆盖则更新现有测试，不重复造大量 case。

---

# T-S04-004：动态高度

浏览器或组件测量：

```text
15 rows → 768px
10 rows → 528px
3 rows  → 192px
0 ready → 192px
```

初始 loading：

- 使用 BeUI SkeletonRows；
- 不出现长期 768px 空白壳。

---

# T-S04-005：行交互 / 状态

### usable

- click 打开详情；
- Enter 打开；
- Space 打开。

### error

- `aria-disabled=true`
- click 不打开；
- Enter / Space 不打开。

### icon

- incomplete → `数据不完整`
- error → `数据计算异常`
- Tooltip 为官方 BeUI 视觉。

---

# T-S04-006：分页 Input 真实 bug

测试必须按真实场景：

```text
page=2
totalPages=5
Input 输入 "3"
Enter
```

断言：

```text
onGoToPage(3) exactly once
rerender page=3
Input value === "3"
```

再加一个必要 invalid case：

```text
输入 99
blur
→ reset "2"
→ 不调用 goToPage
```

不建立更多排列组合。

---

# 7. 必跑命令

在 `frontend/`：

```bash
npm run build
```

相关测试至少：

```bash
npm test -- \
  src/dashboard/session/SessionTable.test.tsx \
  src/dashboard/session/SessionTableFooter.test.tsx \
  src/dashboard/session/useSessionTableController.test.ts
```

如果仓库实际测试文件名不同，使用对应现有文件，不因名字不同新增重复测试。

若同步 BeUI Table / Input / Tooltip 后已有本地 primitive tests，运行被修改 primitive 的现有测试。

无需 Rust tests。

---

# 8. Gate S04

## Gate S04-A：Table 来源

- `@beui/table` 完整官方 baseline 已重新同步。
- 只有 `manualSort`、`getRowProps` 两个 MU 源码扩展。
- 无第三个 Table extension。
- 官方 minTableWidth / Header / SkeletonRows 未被删改。

## Gate S04-B：Table 视觉

- root = 官方 `bg-background`。
- `className` 只保留 `rounded-2xl`。
- header / row / cells 使用官方 visual。
- 1280 / 1440 正常桌面无横向滚动。
- 8 列宽度符合本文。

## Gate S04-C：排序与分页

- 全量 sort_index 排序。
- 15/page。
- 60-row window。
- 第 3 页预取。
- manualSort 阻止当前页二次排序。

## Gate S04-D：Height / State

- rowHeight=48。
- 15/10/3 rows = 768/528/192。
- Empty=192。
- Loading=官方 SkeletonRows。
- Empty 文案正确。

## Gate S04-E：交互

- usable row：click / Enter / Space。
- error row：不可激活 + aria-disabled。
- incomplete/error 只在标题列有状态 icon + 官方 Tooltip。

## Gate S04-F：分页 Input

- Input = 当前官方 BeUI Input。
- compact 只通过 `classNames` slots。
- page2 → 输入3 → Enter → page3，Input 保持3。
- invalid value 正确回退。

## Gate S04-G：工程

```text
npm run build = PASS
Spec04 targeted tests = PASS
```

全部通过才允许进入下一 Spec。

---

# 9. 施工员禁止事项

1. 禁止只 patch 当前 Table fork 而不重新建立官方 baseline。
2. 禁止删除当前官方 Table 的 `minTableWidth` / root-font-size 逻辑。
3. 禁止修改官方 TableHeader 的 Chevron、hover、active、Motion。
4. 禁止把官方 TableHeader 内部 `0.18` 误当 MU magic number清掉。
5. 禁止加入除 `manualSort`、`getRowProps` 之外的第三个 Table 扩展。
6. 禁止业务层 `bg-card` 覆盖 Table root。
7. 禁止用 CSS 自己写 colgroup。
8. 禁止改成当前页排序。
9. 禁止改成无限滚动。
10. 禁止用 `onEndReached` 代替现有分页。
11. 禁止重写官方 Input 为 compact primitive。
12. 禁止新增 `PaginationInput`。
13. 禁止给普通 row 增加 scale / tilt / ripple / left active bar。
14. 禁止 error row 整行变红。
15. 禁止新增重复 formatter。
16. 禁止借 Spec04 修改 Drawer。
17. 禁止以“看起来像 BeUI Table”验收；必须核对官方 baseline + 两个明确 diff。
