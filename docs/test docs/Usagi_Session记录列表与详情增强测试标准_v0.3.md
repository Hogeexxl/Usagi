# Usagi Session 记录列表与详情增强测试标准

> 版本：v0.3  
> 日期：2026-08-12  
> 对应实施方案：`Usagi_Session记录列表与详情增强实施方案_v0.3.md`

---

## 1. 测试边界与精简原则

本文只测试本轮新增或有意改变的 Session 行为，不重复为上一版本已经完成的能力建立独立正向测试。

以下既有能力仅在 Session 新闭环中验证联动结果，不重新拆测试号：

```text
project_kind / projectless global-state
filter-options
Dashboard filter 控件基础能力
Dashboard KPI 卡片调整
Summary event-level filter 基础逻辑
```

测试继续按**故障边界合并**，不按每个字段、按钮、排序方向、响应式宽度拆成独立编号。同一条测试可以用 table-driven / fixture matrix 覆盖多个必要场景。

本版本正式功能测试共 **9 条**：沿用 v0.2 的 8 条核心测试，并新增 1 条 Drawer UI / 交互闭环测试。不得为了覆盖视觉细节继续扩张大量测试条目。

本版本不要求：

- 对每个 Token 字段分别建一个测试；
- 对 Drawer 每个字号、颜色、圆角、动画毫秒值做独立断言；
- 对 Main 每个模型数量建立不同测试；
- 对 Subagent 多模型做测试，本轮明确按每个 Subagent 单模型契约实现；
- 为预估费用建立费用算法测试，本轮费用仅验证未知态 `—` 占位。

---

## 2. Gate 与送测时机

| Gate | 实施阶段 | 正式测试 | 目的 |
|---|---|---|---|
| Gate A | S1–S3 全部完成 | T-S01-001 ～ T-S03-001 | 后端资格、聚合、轻量索引、Row/Detail API 闭环 |
| Gate B | S4–S7 全部完成 | T-S04-001 ～ T-S07-001 | 前端 DTO、全局排序、分页、预取、Session 列表闭环 |
| Gate C | S8–S10 全部完成 | T-S08-001、T-S09-001 + Gate A/B 全部条目 + 受影响回归 | Detail cache/revision、Drawer 与真实页面最终闭环 |

阶段内可运行定向单测辅助开发；正式 Gate 只按上述节点执行，禁止每完成一个小函数就建立一轮正式验收。

---

## 3. S1：Session 资格与聚合语义

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S01-001 | P0 | Gate A | 单一后端矩阵验证：列表只允许 `agent_role=main && root_session_id=thread_id && parent_thread_id IS NULL`；模型单/多选只决定 root 资格且 Main/任意层级 Subagent 命中均可入选；项目条件按 root project/projectless/unknown 资格；模型+项目跨维度 AND；入选后 `self/inclusive/subagent usage`、models、Tree-level `last_activity_at_ms` 均重新按当前 range **全模型**聚合，未选模型 Usage 不得被裁剪；嵌套 Subagent 必须计入全部后代。 |

通过标准：一个 fixture matrix 覆盖上述资格与完整聚合语义，不再拆成 main/subagent/filter 类型多个编号。

---

## 4. S2：轻量排序索引与 Session snapshot

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S02-001 | P0 | Gate A | 构造 >60 个符合资格 root：snapshot 的 `total_items == sort_index.len()`；sort index 每 root 唯一且 total/combined/cache/time/model/project sort 值与完整 Row 口径一致；`items.len() <= 60` 且全部属于 index；同一响应的 revision/total/index/items 来自同一冻结 snapshot；seed sort 只影响首批 Row 选择，不改变完整 index 成员。 |

---

## 5. S3：Row batch、Detail 数据与旧 cursor 契约替换

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S03-001 | P0 | Gate A | API integration 单一矩阵：`session-rows` 的 1..60 个 ID 正确返回当前 scope 合资格 Row，>60/非法 ID/filter 拒绝；model filter 下 Row/Detail 仍返回当前 range 完整 Usage；Detail 顶层 `last_activity_at_ms` 等于整棵 Session Tree 最后活动；Main 多模型时 `main.model_usage[]` 按稳定模型顺序返回真实分组 Usage，`SUM(model_usage.total_tokens) == self_usage.total_tokens`，reasoning 使用真实值且不得把 `self_usage` 重复填入各模型；Detail 返回全部层级且当前 range 有 Usage 的 Subagent，并保留 `parent_thread_id`，每个 Subagent 本轮只返回一个 `model` 与自身完整 `usage`；`cache_write_tokens=null` 保持未知，`estimated_cost=null` 保持空值；过期 expected revision 返回 `STALE_DATA_REVISION`；`/sessions` 不再包含 `next_cursor`，旧 Session cursor/load-more HTTP 契约不再存在。 |

说明：Main 按模型聚合与 Subagent Detail 在同一个 Detail fixture 中验证，不为每个模型或每个 Subagent 单独拆测试。预估费用只验证 `null` 契约，不验证费用计算。

Gate A 通过后再进入前端阶段。

---

## 6. S4：Frontend DTO / Client

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S04-001 | P0 | Gate B | Client/DTO 单一矩阵：Session snapshot 正确解析 `total_items/sort_index/items`；batch rows 最大 60、ID 与现有 canonical filters 正确编码；Detail 正确解析 Tree-level `last_activity_at_ms`、Main `model_usage[]/self_usage/inclusive_usage`、Subagent 单 `model`、`parent_thread_id`、真实 `reasoning_tokens`、nullable `cache_write_tokens/estimated_cost`；`STALE_DATA_REVISION` 可稳定识别；旧 `next_cursor` DTO/client path 已删除。 |

---

## 7. S5–S6：Controller 分页、全局排序、缓存与预取

S5/S6 高度耦合，合并送测，不分别建立重复 controller 测试。

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S05-001 | P0 | Gate B | Controller 主闭环：200 条 index + 首批 60 Row 时得到 14 页、每页只输出 15 Row；page1→page2 不请求；直接跳 page6 根据当前 sort index 定位 61..120 窗口并只补缺失 Row；filter/range 改变 page=1 且保留 sort；不同 QueryKey 不得串用 Row cache。 |
| T-S06-001 | P0 | Gate B | 全局排序/预取闭环：只给 60 个完整 Row 但提供全部 index 时，六个可排序字段必须按全部 root 排序而不是 60 条局部排序；ASC/DESC、换列默认方向、root ID tie-break、null/空值末尾规则正确；排序变化保持当前页；page3/page7 只预取下一 60-rank 窗口且不会递归拉全量；已缓存 Row 可在同 scope/revision 的不同排序间按 ID 复用。 |

---

## 8. S7：Session 列表展示

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S07-001 | P1 | Gate B | 组件/浏览器矩阵：列表只渲染 8 列「最后活动、标题、项目、模型、总 Token、合计 Token、缓存命中率、合计费用」；输入/输出/推理列和“加载更多”消失；六个可排序表头显示方向并调用 controller sort；分页显示总条数/当前页/总页，上一页/下一页/合法数字跳页正确且无 page-size 控件；总/合计 Token 分别来自 `self/inclusive`，Token 文本使用完整千分位整数，不出现 K/M/B。 |

Gate B 通过后再接入 Detail / Drawer 最终闭环。

---

## 9. S8：Detail controller、cache 与 revision

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S08-001 | P0 | Gate C | Detail controller 闭环：首次点击 Row 才请求 Detail；同 scope/root/revision 再开命中 cache；切到更高 `data_revision` 后旧 cache 失效，晚到旧 response 不得覆盖新状态，Drawer 打开时可取得并原位使用新 Detail；Detail 继续遵循当前 range，filters 只验证 root 资格不裁剪内容；controller 正确把顶层 Tree-level `last_activity_at_ms`、Main `model_usage[]`、Subagent 单模型数据交给 Drawer；Token formatter 使用完整整数，`cache_write=null` 与 `0` 保持可区分，`estimated_cost=null` 保持未知态。 |

本条只验证请求/cache/revision 与数据映射，不重复验证 Drawer 具体布局和焦点交互。

---

## 10. S9：Drawer UI / 交互

只保留 1 条正式 Drawer 测试，用一个组件/浏览器矩阵覆盖必须的 UI 契约。

| ID | 优先级 | 执行点 | 测试条目 |
|---|---:|---|---|
| T-S09-001 | P1 | Gate C | Drawer UI/交互矩阵：点击或 Enter/Space 打开并高亮当前 Row；Header 显示标题、完整 ID、**Tree-level 最后活动**，Detail 加载前后时间口径不改变；合计区只显示「合计 Token / Main / Subagent」；Main 多模型按 `model_usage[]` 一模型一 block，每个 block 显示总/输入/输出/推理/缓存命中率/缓存读取/缓存写入/预估费用 8 项，Reasoning 显示真实值、费用 `null` 显示 `—`；每个 Subagent 只显示一个模型、一个 block，默认只展开最近活动的第一个且可独立展开多个；`cache_write=null` 显示 `—`、真实 0 显示 `0`；首次 loading、error、refreshing 均保持 Drawer 打开且不清空 Dashboard；关闭按钮/Escape/遮罩可关闭并恢复焦点；根节点具备 `role="dialog"/aria-modal/aria-labelledby`，Subagent 展开按钮具备动态 `aria-expanded`；1512px、900px、640px 三个代表宽度无横向溢出，640px 下 Usage 网格为 2 列。 |

精简约束：

- 不为每一种关闭方式单独建测试号；
- 不为每一个 Usage 字段单独建测试号；
- 不对 240ms/180ms 等动画时长做脆弱的精确毫秒断言，只验证 `prefers-reduced-motion` 下状态可立即完成即可；
- 不做像素级截图 diff 作为唯一验收方式，只验证必要结构、可操作性和无溢出。

---

## 11. S10：最终集成与回归

Gate C 必须重跑本文全部 9 条：

```text
T-S01-001
T-S02-001
T-S03-001
T-S04-001
T-S05-001
T-S06-001
T-S07-001
T-S08-001
T-S09-001
```

并运行受影响的既有回归，重点为：

```text
Summary filter 既有测试
project/projectless/unknown 既有测试
usage aggregate / TokenTotals 既有测试
API Session 旧测试中被本轮正式替换的 cursor 契约
revision/SSE controller 既有测试
frontend check/build
```

不得把上一版本全部测试重新复制成本文新增编号。

---

## 12. 建议测试代码落点

| 阶段 | 建议落点 |
|---|---|
| S1–S3 | `src/usage/aggregate.rs`、`src/api/query.rs`、现有 Session/API integration tests |
| S4 | `frontend/src/data/usagiClient.test.ts` |
| S5–S6 | `frontend/src/dashboard/session/useSessionTableController.test.tsx` 或仓库当前等价 controller test |
| S7 | 现有 SessionTable 组件测试 / browser Session list test |
| S8 | Detail client/controller tests |
| S9 | `SessionDetailDrawer` 组件测试 + 现有 browser gate 中的一个 Drawer 场景 |

文件名允许按仓库实际布局调整；测试 ID 与测试语义必须保留。一个测试函数可通过 fixture/table-driven matrix 覆盖同一测试条目的多个场景。

---

## 13. Gate C 必跑命令

```bash
cargo fmt --check
cargo test --all-targets
cd frontend
npm test
npm run check
npm run build
```

若仓库当前已有可执行的 browser gate，则运行现有 browser gate；不要求为了本轮单独引入新的浏览器测试框架。

如果仓库当前基线已配置且稳定通过 clippy，可追加：

```bash
cargo clippy --all-targets -- -D warnings
```

不得通过 `skip/ignored`、删除断言、放宽断言、mock 掉生产聚合逻辑或保留旧 cursor fallback 制造通过。

---

## 14. 验收标准

1. T-S01-001 ～ T-S09-001 共 9 条正式测试全部 PASS。
2. Session 筛选继续复用 Dashboard 既有 filters；模型只筛 Session 资格，Row/Detail 不裁剪未选模型 Usage。
3. 主列表只显示 Main；全部层级 Subagent 只参与 tree 聚合和 Detail。
4. 总 Token=`self_usage.total_tokens`；合计 Token=`inclusive_usage.total_tokens`；不新增数据库派生列。
5. 前端固定 15/page；完整 Row 单批最多 60；全局排序基于完整轻量 index；跳页和第 3 页预取符合实施方案。
6. Detail 仅点击后加载，并按 scope/root/revision 正确缓存；旧 revision response 不污染新状态。
7. Main `model_usage[]` 为真实按模型聚合，模型总 Token 合计等于 `self_usage.total_tokens`；Reasoning Token 使用真实数据，不允许前端伪造或按比例拆分。
8. Drawer Header 的最后活动始终使用整棵 Session Tree 口径；Subagent 本轮按单模型一个 block 实现。
9. 预估费用本轮保持 `estimated_cost=null -> —` 占位；`cache_write=null` 与真实 `0` 可区分。
10. Session/Drawer Token 数值使用完整千分位整数，不使用 K/M/B。
11. Drawer 必要的打开/关闭、loading/error/refreshing、dialog/focus、Subagent 展开和 640/900/1512 响应式行为通过；不要求像素级或动画毫秒级过度测试。
12. 旧 Session cursor/load-more API/UI 已移除，不存在 dual path/fallback。
13. 上一版本 Dashboard/project/filter 基础能力未被重复实现或回退，受影响既有回归与 frontend check/build 全部通过。
