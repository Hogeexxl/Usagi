# Usagi 费用聚合、Subagent 模型归属与 Drawer 优化测试标准

> 版本：v0.1  
> 日期：2026-08-13  
> 对应实施方案：`Usagi_费用聚合_Subagent模型与Drawer优化实施方案_v0.2.md`  
> 代码基线：用户本轮提供的最新 Usagi 代码快照（已完成扫描性能优化）  
> 本文只定义本轮新增或改变行为的**必要测试标准**。生产语义以对应实施方案为唯一依据；不重复 Spec01～06、预估费用模块、扫描性能优化中已经稳定覆盖的全部历史测试。

---

## 1. 测试边界与精简原则

本轮新增/改变的生产行为只有以下五类：

```text
A. codex-auto-review pricing 映射 + pricing v2 历史 reprice
B. owning TurnContext 首条上下文不再丢 model / reasoning_effort + parser v5 历史 rebuild
C. 费用聚合从 all-or-nothing 改为 complete / partial / unknown，并向 API / Frontend 暴露完整性状态
D. Session / Dashboard 对 partial cost 的新展示
E. Session Drawer 合计费用与布局小改动
```

测试条目按**独立故障边界**合并，而不是按文件、函数或每个 UI 文本机械拆分。

一个“测试条目”代表一个可验证的行为或矩阵，**不等于一个 Rust `#[test]` / Vitest `it()`**。允许使用表驱动、参数化或单个 integration test 覆盖同一故障边界下的多个输入组合。

本专项必须遵守：

- 不访问或修改真实 `~/.codex`、真实 `state_5.sqlite` 或真实 `mu.sqlite3` 做自动化写测试；使用临时目录、临时 SQLite 与脱敏/结构等价 rollout fixture。
- owning TurnContext 的真实结构契约不得只用测试自创 JSON；至少一条跨模块 fixture 必须保留真实 Codex rollout 的字段层级与 replay → owning boundary 关系。
- 不为通过旧测试而保留旧 all-or-nothing cost 逻辑、旧 API fallback、dual-read、SQL 猜模型或错误占位。
- 不把 `unknown` 当作真实 Codex 模型名来修；只能修复 MU 能从原始记录确定的上下文丢失。
- 不为本专项新增 scanner 压力矩阵、1GiB 资源测试、SSE/refresh/cursor/timezone 专项、浏览器全分辨率矩阵；这些继续执行仓库已有回归。
- 已完成的扫描性能优化属于本轮不可回退基线。最终 Gate 复跑其既有 Gate D，不重新定义第二套性能阈值。
- 因本轮生产契约变化而失效的旧断言应迁移到新语义；不得通过 skip/ignore、放宽断言或删除正确测试制造 PASS。

本版正式新增 **16 条自动化/静态测试条目**，不新增 P2 专项条目。

---

## 2. 基线版本与冻结契约

本轮完成后的版本关系固定为：

```text
USAGE_PARSER_VERSION:
4 -> 5

USAGE_CANONICAL_ALGORITHM_VERSION:
保持 4
canonical_algorithm_for(5) = Some(4)

COST_ALGORITHM_VERSION:
保持 1

PRICING_CATALOG_VERSION:
1 -> 2
```

Pricing 契约：

```text
gpt-5.6
-> 继续作为 gpt-5.6-sol pricing alias

codex-auto-review
-> pricing 层按 gpt-5.6-luna 计价
-> usage_events.model / API model 字符串不得被改写成 Luna
```

费用聚合契约：

| 参与事件 | `estimated_cost` | `estimated_cost_status` |
|---|---:|---|
| 全部费用已知 | 已知完整 SUM | `complete` |
| 已知 + 未知并存 | 只累加已知费用 | `partial` |
| 有事件且全部费用未知 | `null` | `unknown` |
| 空范围 | `0` | `complete` |

API 对外只允许：

```text
number + complete
number + partial
null   + unknown
```

其他 value/status 组合均为 contract error。

---

## 3. 优先级、施工 Gate 与完成门

| 优先级 | 要求 |
|---|---|
| P0 | pricing/reprice、模型上下文正确性、parser/rebuild、聚合数学、跨视图一致性、API/Client 契约、历史升级。任一失败阻断。 |
| P1 | Session、Dashboard、Drawer 用户可见行为与必要布局。必须通过，但不扩展成额外压力/全分辨率专项。 |

正式 Gate：

| Gate | 施工状态 | 必须完成的测试 |
|---|---|---|
| Gate A — 底层事实 Gate | Pricing、ownership、cost completeness 三个独立底座完成 | `T-MU04-A01`、`T-MU04-B01～B02`、`T-MU04-C01` |
| Gate B — 历史升级 + API 契约 Gate | pricing reprice、parser v5 rebuild、跨聚合/API/Client 契约完成 | `T-MU04-A02`、`T-MU04-B03`、`T-MU04-C02～C03`，并复跑 Gate A |
| Gate C — UI Gate | Session/Dashboard 与 Drawer 两条前端 Track 合并 | `T-MU04-D01～D02`、`T-MU04-E01～E03`，并复跑 `C03` |
| Gate D — 最终集成 Gate | 生产改动冻结，仅允许修复失败点 | `T-MU04-F01～F03` + Gate A/B/C 全部 + 工程命令 + 既有扫描性能 Gate D + 最终真实数据核对 |

前置 Gate PASS 不能由最后一次“全量测试 PASS”替代。Gate 的作用是尽早阻止错误契约向下游传播。

---

## 4. Workstream A — Pricing / Reprice 测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU04-A01 | 独立闭环 | P0 | Gate A | Pricing alias 矩阵：`gpt-5.6-sol`、`gpt-5.6` 均解析到 Sol 既定 rates；`gpt-5.6-luna` 与 `codex-auto-review` 解析到完全相同的 Luna Short/Long rates；`codex-auto-review` 不成为新的 canonical pricing model；真正未知模型仍返回 Unknown，不 fallback 到 Sol/Luna。 |
| T-MU04-A02 | 前置联动：Ledger/open + cost storage | P0 | Gate B | Pricing catalog v1→v2 历史 reprice：旧 active usage 中 `codex-auto-review` 的 NULL cost 启动后按 Luna 价格重算；已有其他已知模型费用保持同公式结果；stored `model` 仍为 `codex-auto-review`；`PRICING_CATALOG_VERSION=2`；reprice 成功后版本/费用原子提交，失败不留下半更新；仅 pricing version 变化不得伪造 usage parser rebuild。 |

---

## 5. Workstream B — Owning TurnContext / Parser v5 测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU04-B01 | 独立闭环 + 前置联动：usage processor | P0 | Gate A | owning boundary 状态矩阵：Subagent replay 后首条 owning `turn_context` 同时承担 ownership boundary 与有效上下文时，必须经现有 parse/normalize/processor 语义初始化 `active_model`、`active_reasoning_effort` 与对应 boundary offset；effort 缺失时明确清空；`SessionMeta` boundary 不伪造 model/effort；boundary 不产生重复 Token event，checkpoint 越过后首条 Token 使用正确上下文。 |
| T-MU04-B02 | 独立闭环 | P0 | Gate A | unresolved 防御边界：Token 真正早于首个 owning model context 时仍允许进入现有 unresolved/`unknown` 防御路径；不得使用后续最终模型、线程 metadata 或 SQL UPDATE 猜测前置 Token；本轮不实现 143 条高可信推断与 110 条模型切换推断。 |
| T-MU04-B03 | 前置联动：scanner/shadow rebuild/epoch | P0 | Gate B | parser v4→v5 历史修复闭环：`USAGE_PARSER_VERSION=5`、canonical algorithm 保持4且 `canonical_algorithm_for(5)=Some(4)`；预置 parser4 active epoch + owning TurnContext 丢上下文 fixture，升级后必须创建/完成 v5 shadow rebuild，旧 active 在激活前继续稳定可读，新 epoch 激活后原本由该 Bug 产生的 `unknown` 恢复为原始 Sol/Terra/Luna + effort，Token/event 不漏不重；原始 Token-before-context 的真正 unresolved 仍可保留。 |

---

## 6. Workstream C — Cost Completeness / Aggregate / API / Client 测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU04-C01 | 独立闭环 | P0 | Gate A | `TokenTotals` / aggregate 费用状态机基础矩阵：全 Known→`complete+SUM`；Known+Unknown（左右顺序均覆盖）→`partial+known subtotal`；全 Unknown→`unknown+None`；Empty→`complete+0` 对外语义；checked add 保持溢出保护。不得把 Unknown event 删除或持久化改成0。 |
| T-MU04-C02 | 前置联动：SQL aggregate/detail | P0 | Gate B | 跨视图聚合一致性：Summary、Model、Session self/subagent/inclusive、Main Detail、Subagent Detail 全部只聚合 event cost，并使用同一 completeness helper；一个未知子项不得把同范围已知 subtotal 抹成 null；全未知仍为 null；`same_totals()` 必须区分“相同金额 complete”与“相同金额 partial”；聚合层不得重新 pricing。 |
| T-MU04-C03 | 前置联动：API + Frontend client | P0 | Gate B / Gate C 回归 | API/Client canonical contract：相关 Token/Summary/Session/Detail DTO 均按既定边界输出 `estimated_cost_status`；仅接受 `number+complete`、`number+partial`、`null+unknown`；字段缺失、未知 status、非法 value/status 组合均被前端 runtime validation 拒绝，不默认 `complete`，不保留旧 response fallback；nanos→USD 精度规则不变。 |

---

## 7. Workstream D — Session / Dashboard UI 测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU04-D01 | 前置联动：C03 | P1 | Gate C | Session 列表费用三态：`complete+number` 沿用正常费用样式；`partial+number` 显示已知 subtotal 且仅费用数字为红色；`unknown+null` 显示 `—`；partial 数字不得新增“部分费用无法估算，当前显示已知费用合计”之类 hover/title 说明。 |
| T-MU04-D02 | 前置联动：C03 | P1 | Gate C | Dashboard「预估费用」KPI：complete 时无警告图标；partial 时费用数字保持当前费用颜色，标题同一行右侧出现红色圆圈叹号；点击图标显示费用不完整气泡，再次点击/点击外部/Escape 可关闭且按钮有必要 aria 状态；unknown 仍显示 `—` 并显示非 complete 警告。不得通过把 KPI 数字本身改红来表达 partial。 |

---

## 8. Workstream E — Session Drawer UI 测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU04-E01 | 前置联动：C03 | P1 | Gate C | Drawer 顶部 Summary 固定为四项：合计 Token / Main / Subagent / 合计费用；合计费用直接使用 Session inclusive cost/status，不在前端重算子项；complete 正常数字、partial 红色 subtotal、unknown `—`；skeleton 同步为4项；窄宽度允许现有规则下 2×2，不产生横向溢出。 |
| T-MU04-E02 | 独立闭环 | P1 | Gate C | 标题数量与复制清理：`Main (x)` 的 x=`detail.main.model_usage.length`，`Subagent (x)` 的 x=`detail.subagents.length`；括号数量左对齐紧靠标题，不再出现“x个模型配置”或右对齐数字；Drawer 内 Session ID/Subagent thread ID 继续显示，但所有「复制」按钮、clipboard helper 与只为 copy 服务的 CSS/断言删除。 |
| T-MU04-E03 | 独立闭环 + 前置联动：现有 formatter | P1 | Gate C | Subagent Header 新布局：左侧只保留 title + thread_id；右侧 meta 上行为现有 `model (reasoning_effort)` 格式、下行为最后活动时间；继续使用既有 mixed/unknown effort 与时间 formatter，不改变数据口径；展开/收起仍可用。现有 browser gate 增加一个窄 Drawer 代表场景，确认右侧 meta 可 wrap 且不造成 Drawer/body 横向溢出，不扩展成新的多分辨率矩阵。 |

---

## 9. Workstream F — 跨模块闭环与最终回归

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU04-F01 | 前置联动：A～E | P0 | Gate D | 单一真实结构闭环 fixture 同时覆盖核心新语义：Subagent replay→owning TurnContext 明确给出 `gpt-5.6-sol/medium`，后续 Token 不再落 `unknown`；另含 `codex-auto-review` usage 可按 Luna 计费；再保留一条真正 unresolved/Unknown-cost 事件。完成扫描/聚合后验证 DB→Aggregate→API→UI：已知费用被正确计入，Session/Drawer 显示 partial 红色 subtotal，Dashboard 显示同一 subtotal + 红色警告图标，stored model 不被改名。 |
| T-MU04-F02 | 前置联动：历史版本 | P0 | Gate D | 同时存在 parser4 + pricing1 的旧库升级闭环：启动新版本后 pricing v2 reprice 与 parser v5 shadow rebuild 各自使用原有独立机制；旧 active 在 rebuild 激活前仍可读且 auto-review 已能获得正确费用；v5 build 使用同一 pricing v2；最终激活后 model/effort 修复、Token 不漏不重、费用不重复计算。不得要求删库，不得把 pricing bump 与 parser bump 合并成同一状态机。 |
| T-MU04-F03 | 前置联动：全仓库 | P0 | Gate D | 受影响全量回归 + 静态收口：执行仓库现有后端/前端/浏览器测试，复跑扫描性能优化测试标准 Gate D；确认生产路径不存在旧 `cost_unknown_count > 0 => None` all-or-nothing 费用语义、SQL 批量猜模型、`codex-auto-review` stored-model 改名、旧 API fallback、Drawer copy button/clipboard 残留；不回退 Usage worklist/exact-plan 性能主路径。 |

---

## 10. 测试代码 ownership 与并行施工约束

为了允许 Luna 多 Track 并行且避免同文件互相覆盖，测试与生产修改必须按以下 ownership 收口。

```text
Track A — Pricing / Reprice
生产：src/cost/pricing.rs、src/cost/mod.rs、既有 cost refresh/reprice 必要调用点
测试：pricing module tests、reprice/storage 对应测试

Track B — Ownership / Parser
生产：src/usage/pipeline.rs、usage parser/version mapping 的必要文件
测试：pipeline/processor/version/rebuild 的局部测试
原则：除 correctness 阻塞外不修改 scanner performance 主 orchestration

Track C — Aggregate / API
生产：src/usage/aggregate.rs、src/api/query.rs
测试：aggregate/API module tests

Frontend Contract Owner（Gate B 尾部的串行 seam）
生产：frontend/src/data/types.ts、frontend/src/data/usagiClient.ts
测试：frontend/src/data/usagiClient.test.ts
说明：API DTO 冻结后一次完成，后续 D/E Track 不再并行修改这两个文件

Track D — Session / Dashboard
生产：MetricGrid.tsx、MetricCard.tsx、SessionTableRow.tsx
测试：对应 Metric/Session row 测试

Track E — Drawer
生产：SessionDetailDrawer.tsx
测试：SessionDetailDrawer.test.tsx

Frontend Integration Owner
唯一负责：frontend/src/index.css 中 D/E 共用或新增样式的最终合并
说明：Track D/E 可以提出 class/样式需求，但不得同时直接改同一 shared CSS 区域

Integration Owner
测试：新建或独占一个本轮跨模块 integration test 文件；统一更新 parser/pricing 全局版本断言与确实冲突的历史 fixture
职责：T-MU04-F01/F02、Gate D 静态检查、测试映射与最终合并
```

关键禁止：

- 两个 Track 同时修改 `src/usage/aggregate.rs`、`frontend/src/data/usagiClient.ts`、`frontend/src/index.css` 等共享热点文件。
- Track B 为方便测试去改 Track A pricing，或 Track C 为方便 UI 去改 Frontend contract。
- Integration Owner 在各 Track 尚未过局部 Gate 时提前“大一统重构”。

---

## 11. Gate 施工/并行顺序

### 11.1 Wave 1 — 三个后端底座并行

可同时启动：

```text
Track A1: pricing alias 本地解析
Track B1: owning TurnContext context fix
Track C1: CostCompleteness / TokenTotals 状态机
```

三者没有生产语义依赖，文件 ownership 也基本独立。

完成后执行 Gate A：

```text
T-MU04-A01
T-MU04-B01
T-MU04-B02
T-MU04-C01
cargo fmt --check
cargo check --all-targets
对应定向 unit/integration subset
```

Gate A 未通过，不允许下游以临时 mock/fallback 绕过错误底座。

### 11.2 Wave 2 — 历史升级、跨聚合/API 并行；Client 作为串行契约 seam

Gate A 通过后可并行：

```text
Track A2: pricing v2 + historical reprice
Track B2: parser v5 + shadow rebuild historical fix
Track C2: SQL aggregate + API estimated_cost_status
```

A2 与 B2 都可能在启动/历史数据上生效，但机制必须独立；它们的并行实现只在 Gate D 的 F02 做最终组合验证，不要求施工时互相调用。

当 Track C2 后端 DTO 冻结后：

```text
Frontend Contract Owner
-> types.ts + usagiClient.ts + client tests
-> 完成 T-MU04-C03
```

这是 Wave 2 唯一刻意串行的接口 seam，目的是避免 D/E 两个前端 Track 各自解释 API。

随后执行 Gate B：

```text
T-MU04-A02
T-MU04-B03
T-MU04-C02
T-MU04-C03
复跑 Gate A
backend 受影响测试
frontend usagiClient tests + npm run check
```

### 11.3 Wave 3 — 两条 UI Track 并行

只有 Gate B 完成、Frontend contract 冻结后才启动：

```text
Track D: Session row + Dashboard KPI warning
Track E: Drawer summary + headings/copy + Subagent header
```

两条 Track 的业务组件不同，可以真正并行。

Shared CSS 由 Frontend Integration Owner 最后一次合并，避免 `index.css` 冲突。

Gate C：

```text
T-MU04-D01～D02
T-MU04-E01～E03
复跑 T-MU04-C03
cd frontend && npm test
cd frontend && npm run check
cd frontend && npm run build
cd frontend && npm run test:browser:gate
```

### 11.4 Wave 4 — Integration only，不再开新生产 Track

Gate C 后停止并行扩展生产改动，只允许按失败归属返回 A/B/C/D/E 对应 Track 修复。

Integration Owner 执行：

```text
T-MU04-F01
T-MU04-F02
T-MU04-F03
Gate A/B/C 全部重跑
工程命令
扫描性能优化既有 Gate D
真实数据最终核对
```

这种顺序避免两个主要风险：

```text
1. parser/reprice 尚未闭环时，前端先围绕错误数据写大量 UI workaround
2. D/E 两个前端 Track 同时修改 client/types/shared CSS，产生难以审计的合并冲突
```

---

## 12. 本版明确不新增的测试

为避免过度设计，本专项不新增：

```text
- 143 条“高可信可推断”早期 Token 的自动模型回填测试
- 110 条模型切换线程的猜测模型测试
- unresolved model confidence/provenance schema 测试
- 新的 1GiB / 大文件压力测试
- 新的 scanner 性能阈值或 benchmark
- 新的 SSE / refresh / cursor / pagination / timezone 专项矩阵
- 新的 Dashboard 全分辨率响应式矩阵
- 新的 Drawer 全设备视觉快照矩阵
- pricing 网络更新/远程 catalog 测试
- 新货币或费用精度测试
```

其中 scanner 性能不新增第二套测试，而是最终复跑当前 `Usagi_扫描更新性能优化测试标准_v0.1.md` 的 Gate D。

---

## 13. Gate D 必跑命令

后端：

```bash
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
git diff --check
```

前端：

```bash
cd frontend
npm test
npm run check
npm run build
npm run test:browser:gate
```

同时执行现有扫描性能优化 Gate D：

```text
T-PERF-001～008 / 对应 Gate A～C 回归
真实 CODEX_HOME 三轮无变化 release 性能验收
```

其硬门槛继续使用既有测试标准，不在本文改写：约 440 present sources、无 build、无数据变化时三轮平均 <= 1.0 秒、任意单轮 <= 1.5 秒，并同时满足既有确定性工作范围计数。

`cargo clippy --all-targets -- -D warnings` 若失败：

- 本轮新增/修改文件中的 warning 必须修复；
- 若最终只剩用户未授权范围内的既有 baseline warning，应在执行记录中明确列为 Gate blocker，不得擅自扩大范围，也不得删除 clippy Gate 或降低 `-D warnings` 制造 PASS。

---

## 14. 最终真实数据核对

自动化 Gate 全部 PASS 后，再使用用户当前真实 Codex 数据做**结果核对**。该步骤不是新的自动化写测试，不修改真实 Codex rollout/state。

至少核对：

1. 原先由 owning TurnContext 丢上下文造成的错误 `unknown` 应随 parser v5 rebuild 消失；不要求数据库所有 unresolved model 绝对为0。
2. 仍存在的 unresolved model 能解释为 Token 早于首个模型上下文等原始数据不足场景，未被强行猜测。
3. `codex-auto-review` 事件获得 Luna 等价费用，模型显示仍为 `codex-auto-review`。
4. 原问题 Session 即使仍包含真正未知费用，也显示已知费用 subtotal；partial Session/Drawer 为红色数字。
5. Dashboard 显示已知费用 subtotal，数字保持原费用色，同时标题右侧显示红色圆圈叹号并可点击查看说明。
6. Drawer 顶部四项、`Main (x)`、`Subagent (x)`、删除复制按钮、Subagent 右侧 model/time 布局均符合实施方案。
7. 完成 rebuild 后再次执行无变化更新，扫描性能不得回退。

---

## 15. 完成判定

本版验收必须同时满足：

```text
T-MU04-A01～A02    PASS
T-MU04-B01～B03    PASS
T-MU04-C01～C03    PASS
T-MU04-D01～D02    PASS
T-MU04-E01～E03    PASS
T-MU04-F01～F03    PASS

Gate A             PASS
Gate B             PASS
Gate C             PASS
Gate D             PASS
```

并满足：

```text
cargo fmt --check                    PASS
cargo check --all-targets            PASS
cargo test --all-targets             PASS
cargo clippy --all-targets -- -D warnings
                                      PASS（或按本文规则明确记录为未授权 baseline blocker；不得伪造 PASS）
git diff --check                     PASS

frontend:
npm test                             PASS
npm run check                        PASS
npm run build                        PASS
npm run test:browser:gate            PASS

扫描性能优化既有 Gate D              PASS
最终真实数据核对                      PASS
```

同时确认：

- `gpt-5.6` 官方 pricing alias 未被删除；
- `codex-auto-review` 只做 pricing 映射，stored model 未改名；
- parser v5 与 pricing v2 是两条独立升级机制；
- canonical algorithm 仍为4；
- 真正 unresolved model 未被猜测回填；
- unknown cost 未被伪装为 `$0.00`；
- partial cost 的已知 subtotal 不再被上层聚合抹掉；
- 前端不自行重新计算 Session/Drawer subtotal；
- 无旧 API fallback / dual-read；
- 无 Session partial hover 说明；
- Dashboard partial 数字未改红；
- Drawer 内复制按钮与无用 clipboard 逻辑已删除；
- 不通过跳过测试、放宽断言、回退扫描性能优化制造通过。

任何 P0/P1 条目失败，本版不得判定完成。
