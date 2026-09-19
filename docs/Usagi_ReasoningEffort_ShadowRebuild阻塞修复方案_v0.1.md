# Usagi Reasoning Effort Shadow Rebuild 阻塞修复方案 v0.1

## 1. 文档目的

本文用于修复当前最新版 Usagi 中已经确认的一个生产缺陷：

```text
Usage Parser / Canonical v3 → v4 后，
shadow usage epoch 无法在部分历史 rollout 上完成 rebuild，
导致新 epoch 无法激活，
Main / Subagent 的 reasoning_effort 最终仍显示（—）。
```

本文同时给出：

```text
1. 已确认根因
2. 最小生产修复范围
3. 现有数据库的恢复方式
4. 必要测试条目
5. Luna 的施工与 Gate 顺序
```

本文是独立热修复方案，不重新实施整个“预估费用 + reasoning effort”模块。

---

# Part A — 已确认现象与根因

## 2. 当前真实运行状态

用户真实数据库已经确认：

```text
active usage epoch = 3
shadow/build epoch = 4

build source 总数 = 437
rebuilt             = 406
pending             = 31
```

这 31 个 source：

```text
- 都是 present source
- 都属于旧 active epoch 的真实贡献者
- active epoch 3 中合计存在真实 usage event
- metadata ownership/root 已经可确定
- v4 usage checkpoint 持续停留在：
    committed_offset = 0
    processing_status = rebuild_required
- 没有持续推进
- 没有显式持久化 error
```

shadow epoch 4 对已经成功 rebuild 的当前 Session 已经正确解析出：

```text
Main:
gpt-5.6-sol / medium

Subagent:
gpt-5.6-luna / max
```

因此以下链路不是本次根因：

```text
turn_context.effort parser
reasoning_effort normalize
UsageEvent.reasoning_effort
Aggregate/API 字段
Drawer formatter
Main/Subagent effort UI
```

当前 UI 显示：

```text
（—）
```

只是因为 API 仍读取 active epoch 3，而 epoch 3 本身是旧 v3 canonical usage 数据，不含 reasoning_effort。

---

## 3. 5 个真实 rollout 的诊断结论

从 31 个 stuck source 中抽取的 5 个代表样本全部命中同一个失败模式，覆盖：

```text
Main
Subagent
较小历史 source
较大历史 source
极少 usage event 的 source
不同 reasoning effort：high / medium / max
```

共同结构为：

```text
同一个 open Turn 跨 usage batch 边界

Batch N：
Turn 已经建立
但还没有足够 usage candidate 用来确认 reasoning effort
→ durable Turn state 写入：
  reasoning_effort_state = none

Batch N+1：
继续读取同一个 Turn
观察到 effort + usage candidate
→ processor 正确演进为：
  Single(high / medium / max)
```

这属于正常的增量读取场景。

---

## 4. 已确认生产缺陷

缺陷位于：

```text
src/storage/usage.rs
write_turn()
```

当前 `turns` UPSERT 的 reasoning-effort compatibility predicate 把状态演进方向写反了。

### 4.1 正确的状态含义

`reasoning_effort_state` 不是用户的 Codex effort 设置本身，而是 MU 对**同一个 Turn 已经观察到的 effort 集合**的摘要：

```text
none
→ 当前还没有观察到可归属的 effort

single("high")
→ 当前只观察到 high

mixed
→ 当前已经观察到多个不同 effort
```

正常单调演进：

```text
none
  ↓
single(x)
  ↓
mixed
```

允许的信息演进：

```text
none       → none
none       → single(x)
none       → mixed

single(x)  → single(x)
single(x)  → mixed

mixed      → mixed
```

禁止退化或覆盖历史事实：

```text
single(x)  → none
mixed      → none
mixed      → single(x)
single(x)  → single(y), x != y
```

注意：

```text
Turn 1 = high
Turn 2 = medium
```

完全合法，因为这是两个不同 Turn。

如果**同一个 Turn**先观察到 high、之后又观察到 medium，正确结果是：

```text
single(high) → mixed
```

而不是：

```text
single(high) → single(medium)
```

---

## 5. 当前 SQL 为什么会失败

当前 `write_turn()` 中的 predicate 以：

```text
excluded.reasoning_effort_state
```

作为主要判断方向。

但在 SQLite UPSERT 中：

```text
turns.*
→ 数据库里同一个 Turn 的旧 durable state

excluded.*
→ 本次 commit 希望写入的新 state
```

当前条件会错误拒绝最正常的：

```text
existing = none
incoming = single("high")
```

于是：

```text
UPSERT UPDATE 影响行数 = 0
↓
changed != 1
↓
StorageError::usage_conflict("usage Turn conflict")
```

该错误被标记为：

```text
requires_usage_rebuild = true
```

scanner 随后按照现有 rebuild 保护逻辑：

```text
commit 失败
↓
replace_or_begin()
↓
当前 source 的 shadow rebuild 结果被清理
↓
checkpoint 重置为：
offset = 0
rebuild_required
↓
本轮返回 Ok
```

下一次扫描再次从 offset 0 开始，并在同一个跨 batch Turn 上再次失败。

因此表现为：

```text
BuildFrom(0)
→ 处理中间其实已经产生进度
→ same-Turn none → single
→ SQL 错误判冲突
→ shadow source 被清理
→ checkpoint 回到 0
→ 下一轮再次重复
```

这就是 31 个 source 长期：

```text
pending + rebuild_required + offset 0
```

且无稳定 error 的原因。

---

# Part B — 生产修复方案

## 6. 修复原则

本次只修：

```text
同一个 Turn 的 reasoning_effort_state durable UPSERT
单调演进方向错误
```

不改变：

```text
Token 计算
reasoning effort parser
canonical event identity
TurnCompensation 数值
scanner rebuild 策略
epoch activation 条件
数据库 schema
migration
cost 算法
API
frontend
```

---

## 7. 修改文件

生产代码只要求修改：

```text
src/storage/usage.rs
```

如测试代码位于同文件内，可同步新增/修改该文件的 test module。

若为了 scanner/rebuild 集成测试必须修改现有 test fixture，可修改对应测试文件，但不得因此扩展生产代码范围。

---

## 8. `write_turn()` 正确修复

修改：

```text
src/storage/usage.rs::write_turn()
```

当前 reasoning-effort predicate 替换为按：

```text
existing turns.* → incoming excluded.*
```

判断单调演进。

目标 SQL 语义：

```sql
AND (
    turns.reasoning_effort_state='none'

    OR (
        turns.reasoning_effort_state='single'
        AND (
            (
                excluded.reasoning_effort_state='single'
                AND turns.single_reasoning_effort
                    = excluded.single_reasoning_effort
            )
            OR excluded.reasoning_effort_state='mixed'
        )
    )

    OR (
        turns.reasoning_effort_state='mixed'
        AND excluded.reasoning_effort_state='mixed'
    )
)
```

其状态矩阵必须为：

| Existing DB state | Incoming state | 结果 |
|---|---|---|
| none | none | 允许 |
| none | single(high) | 允许 |
| none | mixed | 允许 |
| single(high) | single(high) | 允许 |
| single(high) | mixed | 允许 |
| single(high) | none | 拒绝 |
| single(high) | single(medium) | 拒绝 |
| mixed | mixed | 允许 |
| mixed | none | 拒绝 |
| mixed | single(high) | 拒绝 |

建议在 SQL 附近加入简短代码注释：

```text
Reasoning-effort Turn summary is monotonic:
none -> single(same value) -> mixed.
The existing durable state must never be replaced by a less informative state.
```

---

## 9. 以下现有逻辑不得跟着修改

### 9.1 `observe_turn_reasoning_effort()`

当前 processor 语义是正确的：

```text
None + high
→ Single(high)

Single(high) + high
→ Single(high)

Single(high) + medium
→ Mixed

Mixed + any known effort
→ Mixed

Unknown effort
→ unresolved_reasoning_effort_seen = true
```

不修改。

### 9.2 `unresolved_reasoning_effort_seen`

当前：

```sql
turns.unresolved_reasoning_effort_seen
<= excluded.unresolved_reasoning_effort_seen
```

保持不变。

其语义是：

```text
false → true 允许
true  → false 拒绝
```

一旦同一个 Turn 观察过 Unknown effort，就不能在后续 commit 中忘掉该事实。

### 9.3 `carry_turn()`

`carry_turn()` 目前使用：

```text
b = build/较早 partial Turn state
a = active/更完整 Turn state
```

其 reasoning-effort compatibility 方向与 `write_turn()` 的 existing/incoming 方向不同，当前语义是正确的。

**禁止因为本次修复而机械地把 `carry_turn()` 的条件一起反转。**

### 9.4 Token durable state

本次不修改 Token 相关：

```text
accounted_*
last_total_*
start_total_*
accounted_candidate_count
state_through_offset
compensation
```

本次缺陷是新增 reasoning-effort state 的 UPSERT predicate 写反，不是 Token 累积状态机缺陷。

### 9.5 Scanner / epoch

禁止修改：

```text
src/scanner/usage_consumer.rs
activate_rebuild()
replace_or_begin()
all-members-complete activation 条件
```

当前 epoch 原子激活原则是正确的。

---

## 10. 不需要版本升级或 migration

本次修复不改变：

```text
rollout → canonical usage event 的业务语义
event_id 编码
数据库 schema
reasoning_effort 字段定义
```

因此：

```text
USAGE_PARSER_VERSION
保持 4

USAGE_CANONICAL_ALGORITHM_VERSION
保持 4

LATEST_SCHEMA_VERSION
保持 7

不新增 migration
不创建 usage epoch 5
```

这是一个 storage compatibility bug fix，不是新的 canonical 版本。

---

# Part C — 现有真实数据库的恢复行为

## 11. 禁止人工修数据库

不得：

```text
手工 UPDATE usage_active_epoch
手工把 31 个 pending 改 rebuilt
删除 usage_build_sources member
删除 source checkpoint
删除 mu.sqlite3
重新初始化数据库
放宽 epoch activation 完整性条件
```

---

## 12. 修复后的正常恢复路径

当前 31 个 source 已经处于：

```text
checkpoint:
offset = 0
processing_status = rebuild_required
```

代码修复后，应直接让现有 scanner 状态机自然处理：

```text
下一次 usage scan
↓
31 个 source 正常 BuildFrom(0)
↓
跨 batch open Turn：
none → single / mixed
现在能够正常 durable commit
↓
checkpoint 持续前进
↓
completion_status = rebuilt
↓
437 / 437 build member complete
↓
shadow epoch 4 原子激活
↓
active_epoch = 4
```

激活后 API 才应读取 epoch 4。

当前已确认的 Session 最终应出现：

```text
Main
gpt-5.6-sol (medium)

Subagent
gpt-5.6-luna (max)
```

---

# Part D — 必要测试标准

本次只增加修复所需的必要测试，不扩展成新的大规模测试矩阵。

## 13. 测试条目

### T-HF-RE01 — `write_turn()` reasoning-effort 单调状态矩阵

**优先级：P0**

直接验证同一个 Turn 的 storage UPSERT compatibility：

必须 PASS：

```text
none → none
none → single(high)
none → mixed
single(high) → single(high)
single(high) → mixed
mixed → mixed
```

必须仍然产生 `usage Turn conflict`：

```text
single(high) → none
single(high) → single(medium)
mixed → none
mixed → single(high)
```

同时确认冲突时旧 durable Turn row 未被部分覆盖。

**目的：**

直接锁定本次 SQL 方向 bug，防止再次把 existing/incoming 写反。

---

### T-HF-RE02 — 同一个 open Turn 跨两次 commit：`none → single`

**优先级：P0**

使用现有 storage/ledger fixture 构造同一个：

```text
ledger_epoch
source_file_id
file_generation
turn_key
```

第一次 commit：

```text
reasoning_effort_state = none
accounted_candidate_count = N
state_through_offset = X
status = open
```

第二次 commit 同一个 Turn：

```text
reasoning_effort_state = single("high")
accounted_candidate_count > N
state_through_offset > X
status = open
```

要求：

```text
第二次 commit 成功
不返回 usage Turn conflict
数据库最终：
reasoning_effort_state = single
single_reasoning_effort = high

Token/accounted 数据按原规则前进
```

该测试必须是真正的**第二次 UPSERT 同一 Turn**，不能像旧 `T-MU03-C02` 一样只测试：

```text
直接第一次写 Single(high)
→ read back Single(high)
```

---

### T-HF-RE03 — 同一个 open Turn 跨三次 commit：`none → single → mixed`

**优先级：P0**

连续提交同一个 Turn：

```text
Commit 1:
none

Commit 2:
single("high")

Commit 3:
mixed
```

要求全部成功，并最终：

```text
reasoning_effort_state = mixed
single_reasoning_effort = NULL
```

同时验证：

```text
unresolved_reasoning_effort_seen
仍遵守 false → true 单调规则
```

若该 Turn 最终产生 TurnCompensation：

```text
Token compensation 数值仍按原算法生成
reasoning_effort = NULL
```

不得把 mixed compensation 归到最后一次 high/medium。

---

### T-HF-RE04 — Shadow rebuild 跨 batch 回归

**优先级：P0**

建立一个最小可控 fixture，必须制造：

```text
同一个 Turn 跨 usage batch boundary

Batch 1:
Turn 已建立
尚未有可记账 effort usage
→ durable state none

Batch 2:
出现 reasoning_effort + usage candidate
→ state single(...)
```

运行真实：

```text
BuildFrom(0)
→ usage commit
→ 下一 batch
→ usage commit
```

要求：

```text
不得产生 usage Turn conflict
checkpoint 必须 > 0 持续前进
不得被 replace_or_begin() 重置到 0
build source 最终可以完成 rebuilt
```

优先使用小型 synthetic fixture，不把用户提供的几十 MB 真实 rollout 加入仓库。

用户提供的 5 个真实 stuck rollout 可用于本地人工复验，但不应作为正式仓库 fixture。

---

### T-HF-RE05 — 真实数据库恢复 + Token 不回归验收

**优先级：P0，最终验收**

在用户现有 `mu.sqlite3` 上执行，**禁止人工修改 epoch/build/checkpoint 状态**。

修复前记录：

```text
active_epoch = 3
build_epoch = 4
build members = 437
rebuilt = 406
pending = 31
```

同时记录 active epoch 3 的关键 Token aggregate：

```text
total_tokens
input_tokens
cached_tokens
output_tokens
reasoning_tokens
```

修复代码启动/刷新后，等待正常 scanner 完成。

要求：

```text
31 个 source 不再长期停留：
pending + rebuild_required + offset 0

pending 最终 = 0
437 / 437 complete
epoch 4 自动激活
active_epoch = 4
```

然后验证目标 Session：

```text
Main:
gpt-5.6-sol (medium)

Subagent:
gpt-5.6-luna (max)
```

以及：

```text
reasoning_effort 不再为 null
Drawer 不再显示（—）
```

Token 回归要求：

在相同 source/filter 范围下，v4 rebuild 不得因为本次 storage 修复产生 Token 重复或丢失。

重点核对：

```text
total_tokens
input_tokens
cached_tokens
output_tokens
reasoning_tokens
```

若新旧 canonical 中存在历史上已经明确允许的统计差异，必须指出具体既有原因；不得把由本次修复新增的 Token 差异作为正常现象接受。

---

# Part E — Luna 施工顺序与 Gate

## 14. Step 0 — 只读记录基线

修改代码前记录真实数据库：

```text
active/build epoch
437 / 406 / 31 状态
31 个 source 当前 checkpoint
关键 Token totals
目标 Session 当前 API reasoning_effort
```

不修改数据库。

---

## 15. Step 1 — 修复 `write_turn()`

只修改：

```text
src/storage/usage.rs
```

完成：

```text
reasoning_effort_state UPSERT predicate 方向修正
必要注释
T-HF-RE01
T-HF-RE02
T-HF-RE03
```

### Gate 1

必须 PASS：

```text
T-HF-RE01
T-HF-RE02
T-HF-RE03

cargo fmt --check
cargo check --all-targets
```

Gate 1 失败不得进入 scanner/E2E。

---

## 16. Step 2 — Rebuild 回归

完成：

```text
T-HF-RE04
```

### Gate 2

必须确认：

```text
跨 batch none → single 不再 reset
checkpoint 能向前
build source 能完成
```

同时执行相关 usage/storage/rebuild 现有测试。

---

## 17. Step 3 — 用户真实数据库自然恢复

使用用户现有数据库，不做手工 SQL 修补。

启动最新版 MU 或触发正常 refresh/scan。

### Gate 3

执行：

```text
T-HF-RE05
```

如果仍存在 pending source：

```text
停止验收
输出剩余 source_id
输出最新 checkpoint/build state
输出实际失败分支
```

不得：

```text
强制激活 epoch 4
删除 stuck member
手动改 rebuilt
```

---

## 18. Step 4 — 最终回归

必跑：

```bash
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
```

前端代码本次原则上不修改，但仍执行项目现有 frontend 基础回归：

```bash
cd frontend
npm test
npm run check
npm run build
```

如果项目实际 frontend script 名称与上述不同，以当前 `package.json` 已有脚本为准，不新增无关工具。

---

# Part F — 明确禁止事项

## 19. 不允许的“快速修复”

禁止：

```text
1. 修改 epoch activation 条件，让 406/437 也能提前 active
2. 手工把 31 个 source 标记 rebuilt
3. 从 manifest 删除 31 个 source
4. 删除 mu.sqlite3 重新扫描
5. bump parser/canonical version 到 5 来绕过当前 build
6. 新增 schema migration
7. 把 mixed 当作真实 Codex reasoning effort
8. 允许同一 Turn single(high) 直接覆盖成 single(medium)
9. 因此 bug 修改 Token 计算公式
10. 修改 frontend 用 fallback 猜 effort
11. 修改 carry_turn() 的正确兼容方向
12. 放宽测试断言或跳过已有失败测试
```

---

# Part G — 完成判定

本修复只有同时满足以下条件才算完成：

```text
[代码]
write_turn() reasoning-effort monotonic predicate 正确

[专项测试]
T-HF-RE01 PASS
T-HF-RE02 PASS
T-HF-RE03 PASS
T-HF-RE04 PASS
T-HF-RE05 PASS

[真实数据]
31 stuck source 自然收敛
437 / 437 complete
epoch 4 自动激活

[功能]
Main reasoning effort 显示真实 medium
Subagent reasoning effort 显示真实 max
不再显示（—）

[数据安全]
不人工修改数据库
不绕过 epoch 原子激活
Token 无新增重复/丢失

[全量回归]
cargo fmt --check      PASS
cargo check --all-targets PASS
cargo test --all-targets  PASS
frontend npm test/check/build PASS
```

如果专项测试全部通过，但真实数据库仍无法从 31 个 stuck source 收敛，则本修复不得判定完成；必须保留数据库现场并重新定位剩余 source 的实际失败分支。
