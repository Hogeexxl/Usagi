# Usagi Dashboard KPI 与条件筛选测试标准

> 版本：v0.1  
> 日期：2026-08-10  
> 对应实施方案：`Usagi_Dashboard_KPI与条件筛选实施方案_v0.1.md`  
> 格式基线：`Usagi_测试标准_Spec01-06_v0.17.md`

---

## 1. 作用边界与精简原则

本文只验证本轮 Dashboard KPI、`project_kind`、模型/项目条件筛选和已确认 UI 复刻增量。Spec01–06 已有功能直接执行现有正式回归，不在本文重复拆成新条目。

测试按独立故障边界合并：

- 同一函数或同一查询入口的正常、边界和拒绝输入使用一个矩阵测试，不按每个枚举值拆条目；
- 同一用户操作链使用一个 controller/browser 闭环，不按每次点击拆条目；
- parser、aggregate、API、controller、browser 分层保留必要 seam，不重复验证相同事实；
- 本轮没有实现的工具/终端筛选和 Session 表头排序不建立正向功能测试，只在 UI 边界条目中确认其保持不存在。

本文共 20 条测试。禁止因为一个断言包含多个同层输入就再次拆分编号。

## 2. 优先级与批次完成门

| 优先级 | 要求 |
|---|---|
| P0 | migration、项目归属、聚合口径、API 契约、请求竞态和数据隔离。任一失败阻断后续 Gate。 |
| P1 | KPI、筛选交互、字体与 1512px UI、既有 Session 行为保持。对应 Gate 必须通过。 |

只允许在下列批次完成门运行正式测试：

| Gate | 实施阶段 | 运行范围 | 完成条件 |
|---|---|---|---|
| Gate A | S1–S3 全部完成后 | T-S01～T-S03 | schema、adapter、resolver 和 metadata 持久化形成完整闭环。 |
| Gate B | S4–S6 全部完成后 | T-S04～T-S06 | aggregate、filter-options 和 HTTP query 契约形成完整闭环。 |
| Gate C | S7–S9 全部完成后 | T-S07～T-S09 | client、controller、KPI 和 UI 形成完整前端闭环。 |
| Gate D | S10 | T-S10 + 前三批全部条目 + 既有受影响回归 | 真实浏览器闭环和全量回归通过。 |

不得在 S1～S9 内完成一个小点或一个章节后立即运行正式测试。一个 Gate 内先完成全部实现和静态检查，再集中运行该 Gate 的测试。Gate 失败时只修复该批次并重跑失败目标及该 Gate，不提前运行后续 Gate。

## 3. S1–S3：Schema、global-state 与 metadata

| ID | 依赖分类 | 优先级 | 执行点 | 实施方案映射 | 测试条目 |
|---|---|---:|---|---|---|
| T-S01-001 | 独立闭环 | P0 | Gate A | S1；4.1 | v4→v5 migration 矩阵：非空 path backfill 为 `project`、空 path 为 `unknown`；`project_kind` CHECK 拒绝非法值；原 project/token/usage 数据不变；fresh-open、重复打开和失败回滚满足现有 migration 原子性。 |
| T-S01-002 | 前置联动：S2/S3 | P0 | Gate A | S1；4.2–4.3 | `ProjectKind` 贯穿 domain/patch/storage stable projection；分类变化恰当推进一次 `data_revision`，无变化不推进；projectless 保留真实 `project_path/project_name`。 |
| T-S02-001 | 独立闭环 | P0 | Gate A | S2；3.2、5.1–5.2 | global-state adapter 单一矩阵覆盖 complete/not_present/malformed/unreadable、重复 ID、assignment conflict 输入和无关敏感字段；输出只含必要 typed facts，diagnostic/log 不回显原始正文。 |
| T-S03-001 | 前置联动：S1/S2 | P0 | Gate A | S3；3.1、3.3 | resolver 分类矩阵一次覆盖普通项目、显式 projectless+非空 generated cwd、无 path unknown、projectless+assignment conflict unknown、禁止 path heuristic；root/subagent 只按 root metadata 归属。 |
| T-S03-002 | 前置联动：S1/S2 | P0 | Gate A | S3；3.4、5.3 | global-state unavailable 时已有可靠分类保持、新 thread 为 unknown；恢复成功后可纠正并发布 revision；一轮只读一次 snapshot，重复扫描幂等。 |

## 4. S4–S6：Aggregate、options 与 API

| ID | 依赖分类 | 优先级 | 执行点 | 实施方案映射 | 测试条目 |
|---|---|---:|---|---|---|
| T-S04-001 | 前置联动：S1–S3 | P0 | Gate B | S4；7.2–7.4、8.1 | 模型筛选矩阵：单选、多选 OR、重复/乱序 canonicalize、同 Session 多模型只按 event 粒度聚合；Token、reasoning、cache hit 和 distinct root `session_count` 均来自筛后事件。 |
| T-S04-002 | 前置联动：S1–S3 | P0 | Gate B | S4；7.2–7.4、13 | 项目筛选矩阵：单/多 path、projectless、unknown/missing root、path+special OR；subagent usage 归 root 项目，projectless generated path 不重复计数。 |
| T-S04-003 | 前置联动：S1–S3 | P0 | Gate B | S4；7.2、8.1 | 组合语义：模型维度 OR、项目维度 OR、跨维度及日期 AND；空 filter 与当前 Summary 完全等价，Session 聚合/列表查询不受条件筛选影响。 |
| T-S05-001 | 前置联动：S1–S4 | P0 | Gate B | S5；6、8.2 | `filter_options_snapshot()` 单一矩阵：active epoch 全历史 distinct models；普通项目按 path 去重且同名异路径保留；projectless/unknown typed special；无 usage 不入选项；options 与 `data_revision` 同一 SQLite snapshot。 |
| T-S06-001 | 前置联动：S4/S5 | P0 | Gate B | S6；7.1、7.5 | HTTP/query 契约矩阵：repeated model/path、特殊 flags、Unicode/空格/`&` path percent-encode、重复值 canonicalize；空值、控制字符和非法 flag 返回稳定 4xx。 |
| T-S06-002 | 前置联动：S4/S5 | P0 | Gate B | S6；6.1、8.3、9.2 | `filter-options` typed response 与 Summary 无筛选兼容；无筛选 URL 只含 range；既有 `/api/usage/models` 不变；`/api/usage/sessions` 不接收新 filter/sort 参数。 |

## 5. S7–S9：Frontend、KPI 与 UI

| ID | 依赖分类 | 优先级 | 执行点 | 实施方案映射 | 测试条目 |
|---|---|---:|---|---|---|
| T-S07-001 | 前置联动：S5/S6 | P0 | Gate C | S7；9 | client/DTO 矩阵：合法 filter-options union 解析、非法结构拒绝；Summary query 正确序列化 typed special 和 repeated values；空 filter 不发送空参数；选择顺序不同得到同一 canonical key。 |
| T-S07-002 | 前置联动：S6 | P0 | Gate C | S7；10 | controller 闭环：range 与 filters 独立；切日期保留 filters、改 filters 保留 range、「清除筛选」保留日期；query snapshot 隔离、旧请求取消/晚到丢弃、revision 重取使用当前 query；Session controller 不重载。 |
| T-S07-003 | 前置联动：S5/S6 | P1 | Gate C | S7；11 | options 生命周期闭环：mount 一次；打开/选择/切日期/清除不重取；revision 只标脏；一个 scan 周期 completed/failed 终态最多刷新一次；失败保留旧 options/filters，消失的已选值不被静默删除。 |
| T-S08-001 | 前置联动：S7 | P1 | Gate C | S8；12 | KPI 单一渲染矩阵：缓存写入卡移除但 DTO 仍解析；推理 Token 使用既有字段/formatter；无模型 8 张、模型激活隐藏会话数量、仅项目保留、模型+项目仍隐藏；其他 KPI 口径不变。 |
| T-S09-001 | 独立闭环 | P1 | Gate C | S9；14.1–14.2 | 1512px typography/browser matrix：真实 JetBrains Mono 400/500/700 或 variable font 已加载且不伪粗；Dashboard、同步按钮、时间、KPI、Session 标题/表头/正文的 computed style 一次性匹配第 14.2 节。 |
| T-S09-002 | 前置联动：S7 | P1 | Gate C | S9；14.3–14.5 | 1512px 筛选器浏览器闭环：只显示模型/项目；8px flex-wrap gap；idle/active trigger、图标、计数、弹层、checkbox、GPT 父子全选/部分态、项目 typed options、外部点击/Escape、连续多选和「清除筛选」符合契约并即时更新 Summary。 |
| T-S09-003 | 独立闭环 | P1 | Gate C | S9；14.6 | Session UI 保持矩阵：新文字层级正确；9 列、布局、分页、cursor、默认服务端顺序不变；表头保持静态，无箭头、sort 参数或点击重排。 |

## 6. S10：最终集成与回归

| ID | 依赖分类 | 优先级 | 执行点 | 实施方案映射 | 测试条目 |
|---|---|---:|---|---|---|
| T-S10-001 | 前置联动：S1–S9 | P0 | Gate D | S10；17 | 真实 Axum + production frontend 浏览器闭环：无筛选、模型、普通项目、projectless+非空 cwd、unknown、组合筛选、切日期和清除；KPI/API 一致，会话数量显隐正确，Session 请求/内容/分页始终不变。 |
| T-S10-002 | 前置联动：S1–S9 | P0 | Gate D | S10；16–17 | 最终回归门：运行本文全部 20 条、既有 Spec01–06 受影响 P0/P1、migration、scanner、aggregate、API、frontend check/unit/build/browser gate；静态确认无 fallback/dual-read、工具/终端筛选、Session 排序或 Regular 伪粗体。 |

## 7. 建议测试代码落点

| 阶段 | 主要落点 |
|---|---|
| S1–S3 | `src/storage/migrations.rs`、`src/storage/metadata.rs`、`src/codex/global_state.rs`、metadata resolver/scanner integration tests |
| S4–S6 | `src/usage/aggregate.rs`、`src/api/query.rs`、`src/api/tests/dashboard_filters.rs` |
| S7–S9 | `frontend/src/data/usagiClient.test.ts`、`useDashboardController.test.tsx`、`MetricGrid.test.tsx`、`frontend/tests/browser/dashboard.spec.ts` |
| S10 | 现有真实 Axum browser gate 与 Spec01–06 正式回归命令 |

文件名可随仓库现有测试布局调整，但 ID、Gate 和测试语义必须保留。一个测试函数可以覆盖同一条目内的矩阵，不要求每个矩阵行建立独立函数。

## 8. Gate D 必跑命令

```bash
cargo fmt --check
cargo test --all-targets
cd frontend
npm test
npm run check
npm run build
npm run test:browser:gate
```

环境具备 clippy 时追加：

```bash
cargo clippy --all-targets -- -D warnings
```

## 9. 完成判定

- T-S01-001～T-S10-002 共 20 条全部 PASS，不以 skip/ignored 代替；
- Gate A/B/C 均只在对应阶段组合完成后执行，Gate D 才执行全量回归；
- project/projectless/unknown、模型 event 粒度、root/subagent 归属和无筛选兼容全部正确；
- Dashboard 条件筛选只影响 KPI，Session API、分页、内容和默认顺序保持现状；
- 1512px UI 使用真实 JetBrains Mono 字重并符合实施方案第 14 节；
- `cache_write_tokens` 只隐藏卡片，既有 storage/API/client 字段链仍完整；
- 不存在本轮禁止的 fallback、dual-read、工具/终端筛选或 Session 排序实现。
