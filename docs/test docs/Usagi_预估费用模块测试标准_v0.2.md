# Usagi 预估费用模块测试标准 v0.2

> 对应实施方案：`Usagi_预估费用模块实施方案_v0.5.md`  
> 代码基线：用户于 2026-08-12 提供的最新 Usagi 源码快照 `8701431b-11c4-4127-a130-ad3f334a9422.zip`  
> 本文只定义该实施方案新增或改变行为的**必要测试标准**，生产语义以 `Usagi_预估费用模块实施方案_v0.5.md` 为唯一依据；不重复现有 Spec01～06 已稳定覆盖的全部历史测试。  
> 测试执行结果必须另写执行记录；不得继承旧版本 PASS。与本版生产行为冲突的旧占位断言（例如 `estimated_cost` 恒为 `null`、Main Detail 只按 `model` 分组）必须同步更新或删除，不得反向约束生产实现。

---

## 1. 当前基线与强制原则

本版最终生产契约固定为：

```text
Schema:
5 → 6 → 7

METADATA_PARSER_VERSION:
2 → 3

USAGE_PARSER_VERSION:
3 → 4

USAGE_CANONICAL_ALGORITHM_VERSION:
3 → 4

COST_ALGORITHM_VERSION:
1

PRICING_CATALOG_VERSION:
1
```

新增能力：

```text
A. Subagent agent_path 标题适配

B. request/event 级 estimated cost
   - input
   - cached input
   - cache write
   - output
   - Short / Long Context
   - 历史 reprice/backfill

C. reasoning_effort usage 维度
   - rollout turn_context → usage event
   - source state + open Turn durable context
   - rebuild state proof 包含 effort
   - Main Drawer 按 (model, reasoning_effort) 分组
   - Subagent 保持单 block，显示 Single / Unknown / Mixed
```

测试必须遵守：

- 不访问或修改用户真实 `~/.codex`、真实 `state_5.sqlite`、真实 `mu.sqlite3`；
- 使用临时目录、临时 SQLite 和脱敏/结构等价 rollout fixture；
- 不为通过旧测试而保留旧生产逻辑、dual-read、fallback 或错误占位；
- `agent_path`、`reasoning_effort`、费用三条职责链独立验证，最终再做一次跨模块闭环；
- 不新增与本版无关的压力测试、UI 响应式矩阵、极端组合测试；
- 已有 scanner、分页、SSE、cursor、隐私等成熟能力只执行现有全量回归，不在本文重复设计专项用例。

---

## 2. 优先级、施工 Gate 与完成门

| 优先级 | 要求 |
|---|---|
| P0 | 数据口径、迁移、canonical identity、durable state、费用计算、历史重建、聚合/API 契约。任一失败阻断本版验收。 |
| P1 | Drawer 展示和兼容性。必须通过，但不扩展成布局/压力专项。 |

本文只定义测试内容；具体施工 ownership 和并行关系以 `Usagi_预估费用模块实施方案_v0.5.md` Part E 为准。

测试执行点固定为：

| Gate | 必须完成的测试 |
|---|---|
| Wave 1 Gate | `T-MU03-C01`、`T-MU03-C03`、`T-MU03-C04`、`T-MU03-B01～B03`；A1/F1 执行各自局部 unit subset |
| Wave 1.5 Gate | `T-MU03-A01`、`T-MU03-C02`、`T-MU03-S01` |
| Wave 2 Gate | `T-MU03-A02`、`T-MU03-B04`、`T-MU03-B05`，以及 B06/C05 当前可执行的 backend 部分 |
| Wave 3 Gate | `T-MU03-A03`、`T-MU03-B06`、`T-MU03-C05`、`T-MU03-C06`，并复跑 `B02/B03/C03` |
| Wave 4 Gate | `T-MU03-S02`、`T-MU03-S03`、`T-MU03-F01～F03` + 全量回归 |

原则：

```text
局部 Gate
→ 尽早阻止错误接口向下游传播

durable/schema Gate
→ 确认 migration 已正式注册后再让 D1/E1/A2 施工

集成 Gate
→ 验证 Storage/Aggregate/API/Frontend 契约

最终 Gate
→ 历史升级 E2E + 现有全量回归
```

最终全量 PASS 不能替代前置 Gate。

## 3. Workstream A — Subagent `agent_path` 标题测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU03-A01 | 独立闭环 | P0 | Wave 1.5 Gate | `agent_path` 两个输入源正确进入 metadata：state `threads.agent_path` 可选列能读取；rollout `payload.agent_path` 与 `source.subagent.thread_spawn.agent_path` 能解析并按既定优先级合并；旧 state schema 无该列时自然为 `None`；rollout fact 经 `to_safe_fact → DB → from_safe_fact` 后 `agent_path/provenance/offset` 不丢失；`metadata.rs` 与 `source.rs` 两条 safe-fact SQL 路径一致，`agent_path_record_offset` 不得超过 `resolved_through_offset`。 |
| T-MU03-A02 | 独立闭环 | P0 | Wave 2 Gate | Subagent 标题 resolver 口径：`state.name > state.title > session_index.thread_name > state.agent_path > rollout.agent_path > None`；`/root/gate_b_rereview → Gate b rereview`；Main Session 不使用 `agent_path`；`agent_nickname` 不参与；state 暂时不可用时，低优先级 rollout fallback 不覆盖已有明确 title。 |
| T-MU03-A03 | 前置联动：migration/scanner | P0 | Wave 3 Gate | Metadata parser v2→v3 历史重建闭环：预置旧 metadata checkpoint/safe fact，升级后重新读取历史 `session_meta`，将原 `threads.title=NULL` 的 Subagent 根据 `agent_path` 修复为派生标题；Detail API 直接返回该 canonical title，前端不需要额外 `agent_path` fallback。 |

---

## 4. Workstream B — 预估费用测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU03-B01 | 独立闭环 | P0 | Wave 1 Gate | `PricingRepository / ModelPricing` 基础契约：GPT-5.6 Sol/Terra/Luna 能解析当前 catalog 的 Short/Long rates；`gpt-5.6` alias 解析到既定 canonical model；未知模型返回 Unknown，不使用其他模型价格 fallback。 |
| T-MU03-B02 | 独立闭环 | P0 | Wave 1 Gate | CostEstimator 完整公式：`uncached = input - cached - effective_cache_write`；`cache_write_tokens=Some(v)` 正常收费；`cache_write_tokens=None` 仅在费用算法内部按 0 参与估算且 canonical 仍保持 `None`；reasoning 不在 output 之外重复收费；算术使用 checked 路径。建议至少用一组固定向量验证四项分项费用及 total。 |
| T-MU03-B03 | 独立闭环 | P0 | Wave 1 Gate | Short/Long 判定边界与粒度：存在 LongContextPolicy 时 `272000 → Short`、`272001 → Long`，完整 request 使用对应 rates；Normal/Recovered 作为 RequestScoped 处理；TurnCompensation `input<=threshold` 可按 Short，`input>threshold` 返回 `Unknown(AmbiguousLongContextGranularity)`；不得用 Session/Turn/聚合 Token 重新判断。 |
| T-MU03-B04 | 前置联动：storage | P0 | Wave 2 Gate | event 持久化契约：新 event 写入 `estimated_cost_nanos_usd`；Known 写非负整数、Unknown 写 NULL；carry/rebuild/local replay 等复制 event 的路径同步携带 cost；修改 pricing 后 event identity/canonical fingerprint 不变化。 |
| T-MU03-B05 | 前置联动：Ledger::open | P0 | Wave 2 Gate | 历史 reprice/backfill：cost/pricing version 不匹配时 `Ledger::open()` 使用同一 PricingRepository + CostEstimator 对已有 usage events 重算，全部成功后一次事务更新费用、version 与 data revision；任一非法 canonical row/溢出时整体回滚；仅 pricing catalog version 变化不触发 usage parser rebuild。 |
| T-MU03-B06 | 前置联动：Aggregate/API | P0 | Wave 3 Gate | 费用聚合/API 语义：全部 event cost Known → 正确 SUM；任一 event cost NULL → 该聚合范围 `estimated_cost=null`；空 usage 范围 → `0.0`；Summary、Model、Session self/subagent/inclusive、Main `(model, effort)` block 和 Subagent usage 均只聚合 event cost，不重新定价；API 仅在边界把 nanos 转为 USD number/null。 |

固定公式向量建议：

```text
model = gpt-5.6-sol / Short
input_tokens = 1000
cached_tokens = 200
cache_write_tokens = Some(100)
output_tokens = 50
reasoning_tokens = 20

uncached = 700

input cost       = 700 × 5000  = 3,500,000 nanos
cached cost      = 200 × 500   =   100,000 nanos
cache-write cost = 100 × 6250  =   625,000 nanos
output cost      =  50 × 30000 = 1,500,000 nanos

total = 5,725,000 nanos USD
      = 0.005725 USD
```

该向量同时验证 `reasoning_tokens=20` 不额外收费。

---

## 5. Workstream C — Reasoning Effort 测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU03-C01 | 独立闭环 | P0 | Wave 1 Gate | rollout `turn_context` effort 解析：优先 `payload.effort`，仅缺失时允许 `payload.reasoning_effort` fallback；trim + lowercase；空值/control character 安全降级为 Unknown；新的 owning `turn_context` 未提供 effort 时必须清为 `None`，不能继承上一 Turn 的 effort。 |
| T-MU03-C02 | 前置联动：processor/storage/rebuild | P0 | Wave 1.5 Gate | durable effort 跨 checkpoint/restart/carry：`active_reasoning_effort + offset` 与 source state 一起持久化/恢复；open Turn 的 `reasoning_effort_state/single_reasoning_effort/unresolved_reasoning_effort_seen` 能完整 round-trip；非零 offset 续读在下一个 `turn_context` 前生成 event 仍继承正确 effort；Turn compensation 在重启前后归属一致；carry/seed compatibility 不丢 Turn effort；`active_state_fingerprint` 包含 effort+offset 且使用新 proof domain tag。 |
| T-MU03-C03 | 独立闭环 + 前置联动：canonical | P0 | Wave 1 Gate | reasoning effort 成为 canonical event context：相同 model/token/锚点但不同 effort 的 event payload/ID 不得相同；同一原始 request 重放 ID 稳定；`estimated_cost` 不参与 identity；usage parser/canonical 当前版本均为 v4。 |
| T-MU03-C04 | 独立闭环 | P0 | Wave 1 Gate | TurnCompensation effort 归属保护：Turn 内只观察到同一个已知 effort → compensation 归该 effort；多个已知 effort、已知+Unknown、只有 Unknown → `reasoning_effort=None`；不得把 mixed/unknown 归最后一次 effort，也不得因为 effort Unknown 而阻止原本合法的 Token compensation；现有 Token 补偿数值算法不变。 |
| T-MU03-C05 | 前置联动：Aggregate/API/Frontend | P0 | Wave 3 Gate | Main Detail 按 `(model, reasoning_effort)` 分组：同一模型 `high` 与 `medium` 分成两个 block，同一组合合并，NULL effort 为独立 Unknown bucket；每个 block 的 Token/Reasoning/Cache/Estimated Cost 只来自自己的 events；Dashboard model filter、全局 Models API、`models_used` 继续保持 model-only。 |
| T-MU03-C06 | 前置联动：Aggregate/API/Frontend | P1 | Wave 3 Gate | Subagent 保持单 usage block：单一 effort 显示 `model (high)`；无已知 effort 显示 `model (—)`；多个 effort 或 known+NULL 混合显示 `model (mixed)`；不得因 effort 再拆 Subagent block；Main 区块计数文案为“N 个模型配置”。 |

---

## 6. Shared Schema / Version / 历史升级测试

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-MU03-S01 | 独立闭环 | P0 | Wave 1.5 Gate | migration 链：fresh DB 能完整迁移到 latest v7；现有 v5 DB 能依次执行 `0006_subagent_agent_path` 与 `0007_usage_context_and_estimated_cost`；最终 `user_version=7`。v6 的 agent-path fact 三元组、v7 的 `usage_events` effort/cost、`usage_source_states` active effort、`turns` effort state、`app_meta` cost/pricing version 均存在且 CHECK/PK/FK 保持；旧数据无损，重复打开幂等，迁移失败整体回滚。 |
| T-MU03-S02 | 前置联动：scanner | P0 | Wave 4 Gate | 版本升级行为彼此独立：metadata v2→v3 只触发 metadata 历史适配重建；usage parser/canonical v3→v4 通过现有 shadow epoch rebuild 重建 reasoning effort；费用 reprice 是独立 derived-metric 流程。不得把三者合并成同一状态机或互相冒用 checkpoint。 |
| T-MU03-S03 | 前置联动：usage rebuild | P0 | Wave 4 Gate | 历史 usage v3→v4 重建：从 rollout `turn_context` 恢复历史 `(model, reasoning_effort)`；原始 rollout 没有 effort 的 event 保持 NULL，不使用 `state_5.reasoning_effort`、模型默认值或相邻 Turn 猜测；新 v4 epoch 激活后 API/Drawer 使用新分组且 Token 总量不因新增 effort 维度改变。 |

---

## 7. 端到端必要验收

| ID | 优先级 | 测试条目 |
|---|---:|---|
| T-MU03-F01 | P0 | 单一真实结构闭环 fixture 同时覆盖三个 Workstream：Main 有 `gpt-5.6-sol/high`、`gpt-5.6-sol/medium`、`gpt-5.6-terra/max` usage；Subagent `name/title/session_index` 为空但 `agent_path=/root/gate_b_rereview`，且有 `gpt-5.6-luna/high` usage。完成扫描后验证 DB → Aggregate → Detail API → Drawer：Subagent 标题为 `Gate b rereview`；Main 分成三个模型配置 block；Subagent 显示单 block effort；所有可计价 block 均显示对应非占位费用。 |
| T-MU03-F02 | P0 | 升级旧库闭环：从 schema v5 + metadata parser v2 + active usage parser/canonical v3 的 fixture 启动新版本，自动完成 v6/v7 migration、旧 active epoch cost backfill、metadata v3 rebuild、usage v4 shadow rebuild/activation；升级期间不得要求删库，最终历史 Token 不漏不重、标题/effort/cost 均可查询。 |
| T-MU03-F03 | P0 | 现有功能全量回归：执行仓库现有后端与前端自动化测试，确认本版没有破坏 scanner 增量、Session/root 聚合、Dashboard/Session 列表、refresh/status/SSE 等既有行为；只更新与本版新生产契约直接冲突的旧断言，不通过放宽断言或跳过测试制造 PASS。 |

---

## 8. 测试代码 ownership

为避免并行施工互相覆盖，测试代码按最小职责修改：

```text
A1
→ agent_path parser/safe-fact/metadata storage 的局部测试

B1
→ reasoning parser/processor/source state/Turn state/rebuild proof 的局部测试

C1
→ cost module unit tests

F1
→ usagiClient / SessionDetailDrawer / formatter 相关 frontend tests

D1
→ event cost persistence / Ledger::open reprice tests

E1
→ aggregate/API tests

Integration Owner
→ schema version、parser version、跨 Workstream API/E2E、历史旧断言的统一更新
```

现有测试里 `estimated_cost=null` 不应机械删除：未知模型、Unknown cost 仍然应为 null。只需要移除“费用永远未实现”这一旧占位契约。

## 9. 本版明确不新增的测试

为避免过度设计，本专项**不新增**以下测试类型：

```text
- 新的 1GiB / 大文件压力测试
- 新的 scanner 性能基准
- 新的 SSE / refresh 并发压力矩阵
- 新的浏览器多分辨率布局矩阵
- 新的 timezone / cursor / pagination 专项
- PricingRepository 网络更新测试
- OpenRouter / 用户自定义价格测试
- Subagent 多模型/多 effort 分 block 测试
```

理由：

- 前五类属于现有基础能力，继续由现有测试回归；
- 后三类不在 `Usagi_预估费用模块实施方案_v0.5.md` 的生产范围内。

---

## 10. 必跑命令

后端：

```bash
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
```

前端：

```bash
cd frontend
npm test
npm run check
npm run build
```

如仓库现有统一发布门另有固定命令，应继续执行；本文不为本专项新增新的压力/浏览器 runner。

---

## 11. 完成判定

本版测试验收必须同时满足：

```text
T-MU03-A01～A03      全部 PASS
T-MU03-B01～B06      全部 PASS
T-MU03-C01～C06      全部 PASS
T-MU03-S01～S03      全部 PASS
T-MU03-F01～F03      全部 PASS

cargo fmt --check         PASS
cargo check --all-targets    PASS
cargo test --all-targets     PASS

frontend:
npm test             PASS
npm run check        PASS
npm run build        PASS
```

并满足以下收口条件：

- 不存在仍要求生产 `estimated_cost` 恒为 `null` 的旧断言；
- Main Detail 不再以 `model` 单独作为 block 唯一分组键；
- `reasoning_effort` 不被拼入 model、不被默认值猜测；
- `agent_path` 不进入 Drawer/API fallback 逻辑，最终标题仍来自 canonical `threads.title`；
- `cache_write_tokens=None` 的 canonical 语义保持不变；
- reasoning token 不重复收费；
- open Turn reasoning-effort state 在 checkpoint/restart/carry 后不丢失；
- usage rebuild state proof 已包含 active effort + offset；
- Long Context 不由 Session/Turn/聚合 Token 判断；
- 历史升级不要求用户删除数据库或重新初始化；
- 不通过跳过测试、放宽断言、保留错误 fallback 制造通过。

任何 P0/P1 条目失败，本版不得判定完成。
