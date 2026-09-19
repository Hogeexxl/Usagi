# Spec08 真实 Codex 适配与旧方案冗余清理实施方案

- 文档版本：v0.2
- 源码基线：`Usagi-1992404`
- 依据：
  - `Spec_02_Codex原始数据与元数据适配_v0.2.md`
  - `Usagi_测试标准_Spec01-06_v0.17.md`
  - `normalizedTokenUsage数据口径.md`
  - `codex rollout数据口径.md`
  - `Usagi_旧方案与旧测试冗余代码审计.md`
  - 3 份真实 rollout + `.codex/state_5.sqlite` + 故障 `mu.sqlite3`
- 改造性质：**替换式改造**
- 禁止：旧 runtime fallback、dual-read、dual-write、compat alias、为了旧测试继续保留 reference implementation
- 当前目标 schema：SQLite v4
- 当前 metadata parser：v2
- 当前 usage parser/canonical：v3/v3

---

# 0. 本轮实施边界：只做 Delta，不重做 Spec01～06

本方案的输入基线是：**用户当前已经完成 Spec07 数据口径改造并通过既有测试的完整代码。**

因此执行模型固定为：

```text
当前代码基线
+ 本 Spec08 明确要求的修改
= 本轮目标
```

Spec01～Spec06 v0.2 是为了消除旧文档冲突而修订的 **current contract**，不是要求重新执行六个 Spec。

Luna 不得：

```text
从 Spec01 开始重新建项目
重新实现已经工作的 scanner / ledger / API / frontend
因为 Spec 文档升到 v0.2 就重写对应模块
在 Spec08 没有要求的区域做架构重构
```

只有以下两种情况允许修改既有模块：

1. Spec08 的某个步骤明确要求修改该模块；
2. Spec08 修改引发回归测试失败，且确认是本轮改动造成的真实回归。

最终跑 Spec01～06 全量测试是 **回归验证**，不是“重新执行 Spec01～06 开发”。

---

# 1. Luna 执行总规则

必须严格按以下顺序执行：

```text
S0 → S1 → S2 → S3 → S4 → S5 → S6 → S7 → S8 → S9 → S10
```

其中 **S4 必须先于 S5**：先删除 runtime 对旧列的读取/投影，再由 S5 migration 物理删列。禁止交换。

每一步完成后先跑对应局部测试；局部不绿不得进入下一步。

禁止：

```text
先改测试让旧代码继续过
为旧字段增加 alias
为旧 DB schema 增加 runtime dual SQL
payload.parent_thread_id 解析失败后静默退回一个伪 parent
把未知值写成 0
修改 0001/0002/0003
保留 usage/carry.rs 只因为旧测试函数还引用它
保留 app_meta dead 字段只因为旧 Spec 写过
```

历史 migration 与 old stable epoch 的只读存在不是 fallback；不要删除用户旧数据。

---

# 2. S0：先同步当前有效文档引用

在开始代码修改前：

1. 将已验收 `Spec02 v0.2` 放入 active docs；
2. 将 `Usagi_测试标准_Spec01-06_v0.17.md` 作为唯一 active 测试标准；
3. 删除 active：
   ```text
   Usagi_测试标准_数据口径改造.md
   ```
4. `v0.15` 移出 active 目录；如要保留，放 `docs/archive/`；
5. Spec06-01、Spec06-02 必须只引用 v0.17；不得保留 v0.15/v0.16 active 引用；
6. Spec04 测试布局删除 `src/usage/carry.rs` 作为 test target 的说明；
7. 数据口径改造旧实施方案归档，不能继续作为当前结构定义。

本次审计又发现两个旧 contract，因此在代码改造前同步做小幅文档修正：

### 2.1 Spec02 §6.4

删除/改写：

```text
app_meta.metadata_parser_version 必须维护
```

改为：

```text
当前 metadata parser 版本由代码常量 METADATA_PARSER_VERSION 唯一定义；
durable source version 只由 source_checkpoints.parser_version 和
rollout_metadata_facts.metadata_parser_version 持久化。
不存在第二个 global app_meta parser-version authority。
```

### 2.2 Spec01 / Spec05 current-contract 修正

明确 current schema v4 删除：

```text
app_meta.metadata_parser_version
app_meta.last_full_import_completed_at_ms
```

Spec05 `/api/status` 删除：

```text
last_full_import_completed_at_ms
```

首次导入状态只能由真实 scan/source/usage epoch 状态判断。

### 2.3 v0.17 测试标准同步

删除任何要求上述两个 dead fields 必须存在/返回的条目，增加：

```text
current schema 不存在两个 dead app_meta 字段
status API 不返回 last_full_import_completed_at_ms
metadata parser version authority 只有 current constant + per-source checkpoint/fact
```

---

# 3. S1：建立 metadata parser v2 的唯一版本源

## 3.1 `src/codex/rollout.rs`

新增：

```rust
pub const METADATA_PARSER_VERSION: i64 = 2;
```

如 scanner 需要，从 `src/codex/mod.rs` re-export。

不要再允许当前 parser version 由 app config 随意传入。

## 3.2 `src/scanner/coordinator.rs`

当前：

```rust
pub struct ScanConfig {
    ...
    pub metadata_parser_version: u32,
}

ScanConfig::new(codex_home, metadata_parser_version)
```

改为：

```rust
pub struct ScanConfig {
    pub codex_home: PathBuf,
    pub interval: Duration,
}

ScanConfig::new(codex_home)
```

删除 config field。

## 3.3 `src/main.rs`

删除：

```rust
ScanConfig::new(..., 1)
```

改：

```rust
ScanConfig::new(...)
```

## 3.4 `src/scanner/mod.rs` / pipeline construction

当前 parser 创建必须显式使用：

```rust
METADATA_PARSER_VERSION
```

测试不能再通过 ScanConfig 把“当前 parser”调成 v1。

模拟旧数据时：

```text
DB checkpoint/fact parser_version = 1
current code = v2
```

---

# 4. S2：实现真实 `payload.parent_thread_id`

## 4.1 raw allow-list

`src/codex/rollout.rs::SessionMetaAllowed` 新增：

```rust
parent_thread_id: Option<String>,
```

解析：

```rust
payload
    .get("parent_thread_id")
    .and_then(Value::as_str)
    .and_then(valid_uuid...)
```

不要把它塞进 `subagent_parent_thread_id`。

二者必须保持不同来源。

## 4.2 Parent provenance

同时修改：

```text
src/codex/rollout.rs::ParentHintProvenance
src/domain.rs::ParentHintProvenance
as_str / TryFrom / string enum mapping
raw fact → domain fact mapping
domain fact → raw resumed state mapping
```

新增：

```rust
SessionMetaParent
```

字符串：

```text
session_meta_parent
```

不得复用：

```text
subagent_source
```

## 4.3 `apply_session_meta()` parent candidates

一个 session_meta 中允许同时存在：

```text
payload.parent_thread_id
source.subagent.thread_spawn.parent_thread_id
forked_from_id
```

不能使用当前 `if ... else if ...` 让低来源永远不进入 conflict 检查。

改成逐候选提交：

```text
if direct parent exists:
    merge session_meta_parent priority 3

if nested subagent parent exists:
    merge subagent_source priority 2

if source/role 明确 subagent && forked_from exists:
    merge forked_from priority 1
```

## 4.4 `merge_candidate()` 冲突规则

当前实现只在同 priority 值不同时 conflict，必须改。

对 parent candidate：

```text
existing.value == incoming.value
    → 不冲突；必要时高 priority 可替换 provenance

existing.value != incoming.value
    → 一律 conflict = true
    → winner 仍由 priority 决定
```

建议不要让 generic `merge_candidate()` 的 cwd/model 语义被意外改变。

优先做法：

```rust
fn merge_parent_candidate(...)
```

专门实现 parent 规则。

不要为了 parent 改坏 cwd candidate 的原规则。

parent winner：

```text
higher priority wins
same priority:
    first trusted winner 保留
    mark conflict
```

无论 higher/lower 关系，只要值不同，都产生：

```text
has_conflict = true
CandidateConflict(parent_thread_id_hint)
```

## 4.5 Agent role

真实：

```json
"source": {"subagent":{"other":"guardian"}}
```

仍必须：

```text
agent_role = subagent
agent_role_provenance = subagent_source
```

不能要求 `thread_spawn` 才能认出 Subagent。

---

# 5. S3：更新 parent/root resolver

`src/codex/metadata.rs` 目标优先级：

```text
P4 state_5 explicit thread_spawn edge
P3 rollout session_meta_parent
P2 rollout subagent_source
P1 rollout forked_from_id
P0 unresolved
```

## 5.1 state edge absent

`state_5.thread_spawn_edges` 无 child 记录只能表示：

```text
state 没提供 parent
```

不能作为：

```text
thread 没有 parent
```

继续使用 rollout candidate。

## 5.2 cross-source conflict

如果 state edge 与 rollout winner 不同：

```text
state edge 胜出
relationship conflict = true
```

如果 direct parent 与 nested/fork 不同：

```text
direct parent 胜出
relationship conflict = true
```

root 遍历用最终 winning direct-parent graph。

## 5.3 Guardian fixture

必须新增脱敏 fixture，结构严格模拟真实数据：

```json
{
  "type": "session_meta",
  "payload": {
    "id": "<guardian>",
    "parent_thread_id": "<parent>",
    "source": {
      "subagent": {
        "other": "guardian"
      }
    }
  }
}
```

fixture 必须保证：

```text
无 source.subagent.thread_spawn.parent_thread_id
无 forked_from_id
state_5.thread_spawn_edges 无 guardian child edge
```

断言：

```text
role=subagent
parent=<parent>
parent provenance=session_meta_parent
root=正确 root
has_conflict=false（没有其他冲突候选时）
```

---

# 6. S4：先删除 runtime 对 dead global projections 的依赖

## 6.1 Domain

删除：

```text
AppState.metadata_parser_version
ScanState.last_full_import_completed_at_ms
```

调整 constructor/validation。

## 6.2 Storage

修改：

```text
src/storage/mod.rs
src/storage/lifecycle.rs
相关 SELECT row indexes
tests
```

不再读取两个列。

## 6.3 API

`StatusResponse` 删除：

```text
last_full_import_completed_at_ms
```

API contract tests 同步。

## 6.4 Frontend

删除：

```text
frontend/src/data/types.ts
  last_full_import_completed_at_ms

usagiClient.ts 对该字段 parser
相关 fixtures/tests
```

UI 当前本来就不使用，不需要 replacement display。

---


## 6.5 本步骤完成门

S4 完成时 SQLite 仍允许停留在 v3。此时应满足：

```text
v3 DB 仍保留两个旧列
+
新 runtime 已不再 SELECT / 映射 / 返回它们
=
安全、可编译、可运行
```

数据库里暂时多出的未使用列不构成兼容层。S4 不得提前执行物理删列。

局部 Gate：

- Rust 编译及受影响 Domain / Storage / API 测试通过；
- Frontend DTO/parser 测试通过；
- 对 v3 DB 执行正常 scanner / Status API 路径无字段索引错误；
- runtime SQL 已不再读取 `app_meta.metadata_parser_version` 与 `last_full_import_completed_at_ms`；
- 不增加 dual-read、schema sniff、fallback 或 `COALESCE` 兜底。

只有 S4 通过后才能进入 S5。

---

# 7. S5：新增 SQLite migration v4（runtime 脱离旧列后再物理删列）

## 7.1 文件

新增：

```text
src/storage/schema/0004_metadata_parent_v2_cleanup.sql
```

修改：

```rust
LATEST_SCHEMA_VERSION = 4;
```

注册 migration 4。

不得修改：

```text
0001_initial.sql
0002_usage_ledger.sql
0003_normalized_token_usage.sql
```

## 7.2 重建 `rollout_metadata_facts`

新 CHECK：

```sql
parent_hint_provenance IS NULL OR parent_hint_provenance IN (
  'session_meta_parent',
  'subagent_source',
  'forked_from_id'
)
```

复制 v3 全部已有 fact，不改变其：

```text
file_generation
metadata_parser_version
parent values
offsets
conflict flags
```

特别是：

```text
metadata_parser_version=1
```

不能在 migration SQL 中 UPDATE 成 2。

## 7.3 重建 `app_meta`

前置条件：S4 已完成，当前 Rust/API/Frontend 已不再读取即将删除的两个列。禁止在本步骤通过临时 dual-read/fallback 维持旧代码。


删除 current columns：

```text
metadata_parser_version
last_full_import_completed_at_ms
```

完整复制其余：

```text
data_revision
status_revision
scan lifecycle fields
followup fields
codex_home_fingerprint
source_binding_status
usage_active_epoch
usage_build_epoch
usage_parser_version
usage_build_parser_version
```

所有原有 CHECK 继续保留。

## 7.4 FK / indexes

迁移前列出所有依赖 `app_meta` / `rollout_metadata_facts` 的：

```text
foreign keys
indexes
triggers
```

重建后逐一恢复。

## 7.5 migration tests

至少：

### M4-01 fresh 0→4

断言：

```text
user_version=4
```

### M4-02 v3→4 real-shaped

seed：

```text
metadata v1 ready safe fact
usage active epoch old stable
usage build epoch
rollout fact old provenance
```

迁移后：

```text
值无损
parser_version 仍为 1
new provenance CHECK 可写 session_meta_parent
dead app_meta columns 物理消失
```

### M4-03 rollback

迁移中途失败：

```text
user_version=3
v3 schema/data 完整
```

---


## 7.6 本步骤完成门

S5 完成后必须同时满足：

```text
runtime 不读取旧列
AND
current v4 schema 物理不存在旧列
```

执行：

```text
PRAGMA user_version;
PRAGMA table_info(app_meta);
```

断言：

```text
user_version = 4
metadata_parser_version          不存在
last_full_import_completed_at_ms 不存在
```

并至少执行一次真实 scanner/storage read path，证明不存在 `no such column`。

---

# 8. S6：metadata stale fact v1 → v2 真重建

这是这次真实事故中不能省略的一步。

## 8.1 planning

当：

```text
checkpoint.parser_version != METADATA_PARSER_VERSION
```

必须：

```text
PlanAction = rebuild from offset 0
old safe fact = mismatch / 不可复用
```

不能只更新 version number。

## 8.2 commit

解析完整 source 后：

```text
new RolloutMetadataFact.metadata_parser_version = 2
new source checkpoint.parser_version = 2
new parent_hint_provenance = session_meta_parent（若真实来源）
```

原子 commit。

## 8.3 unchanged source

即使：

```text
size/mtime/device/inode 未变化
old checkpoint 在 EOF
```

只要 parser mismatch，仍必须重读。

这是本次 P0 regression。

## 8.4 usage side-effect

metadata commit 导致：

```text
parent/root/binding 发生变化
```

时，进入现有 source observation / usage reconcile/build replacement。

不要直接修改 usage checkpoint。

---

# 9. S7：确保 blocked build 能恢复并 activation

针对真实故障链：

```text
old metadata v1
→ subagent parent/root NULL
→ build member blocked
→ build epoch 无法 activate
```

metadata v2 修复后：

1. source metadata 更新；
2. root_session_id 可解析；
3. build manifest 对该 source 重新评估；
4. blocked → pending/rebuilt（或满足现有严格条件时 carried）；
5. 全 manifest member 均 `rebuilt/carried` 后才 activate；
6. activation 单事务：
   ```text
   usage_active_epoch = usage_build_epoch
   usage_parser_version = v3
   build columns clear
   ```
7. Summary/Model/Session API 从新 active epoch 查询。

不得：

```text
发现一个 blocked 就继续永久沿用旧 epoch 而不重试
手工 UPDATE usage_active_epoch
忽略 blocked source 强制 activation
```

---

# 10. S8：删除其余旧方案冗余代码

本步骤在 metadata v2、v4 migration、blocked build recovery 主链已有新测试后执行。这里处理与 schema 切换无强依赖的其余冗余代码。

## 10.1 删除 `usage/carry.rs`

先做 coverage transfer 表。

建议映射：

| reference 行为 | 生产测试落点 |
|---|---|
| fresh multi-phase carry | `tests/spec04_usage_integration.rs` T-S04-037 |
| partial seed atomicity/conflict | `src/storage/usage.rs::persistent_partial_seed...` |
| resume cursor / reopen | `src/storage/usage/tests/spec04_p2.rs` T-S04-049/051 |
| identity/generation/binding mismatch | T-S04-050 |
| present during carry | T-S04-049/050 |
| manifest finalize | T-S04-036/037 |
| unverified tail / required boundary | T-S04-032/037/040 |

如果 reference 单测存在独有断言，先将该断言写到真实 production path 测试。

然后：

```text
rm src/usage/carry.rs
删除 usage/mod.rs::pub mod carry;
```

## 10.2 删除 domain compatibility aliases

按审计文档 A-02 全部删除。

用正式类型修任何编译引用；禁止创建另一批 alias。

## 10.3 删除 processor dead structs

删除：

```text
UsageCheckpoint
UsageStateProof
LoadedUsagePlanState
```

## 10.4 删除 `ScanEvent`

删除 wrapper enum + validate。

## 10.5 usage parser canonical mapping

改：

```rust
canonical_algorithm_for(1) == None
canonical_algorithm_for(2) == None
canonical_algorithm_for(3) == Some(3)
```

旧 active epoch 处理逻辑继续允许“读取旧 stable epoch / 冻结 opaque durable proof”，但：

```text
old parser 不可 begin new canonical write
old parser 不可 carry into current build
```

把模拟 pre-normalized upgrade 的测试从 parser1 改为真实 parser2。

---

# 11. S9：更新统一测试实现映射

唯一标准：

```text
Usagi_测试标准_Spec01-06_v0.17.md
```

本方案执行过程中不得再创建第二份专项测试标准；若实施导致契约再次变化，只能修订统一标准并升版本。

## 11.1 必须新增/修订测试

### Metadata

```text
payload.parent_thread_id only
direct + nested same
direct + nested conflict
direct + fork conflict
state edge + direct same
state edge + direct conflict
no state edge + direct parent
late repeated parent differing
metadata v1 ready EOF → v2 full replay
```

### SQLite v4

```text
fresh v4
v3→v4
rollback
new provenance CHECK
dead columns absent
old fact parser version not forged
```

### Redundancy static Gate

运行时代码：

```text
src/usage/carry.rs 不存在
pub mod carry 不存在
A-02 aliases 不存在
LoadedUsagePlanState 不存在
ScanEvent wrapper 不存在
```

current schema/runtime：

```text
app_meta.metadata_parser_version 不存在
last_full_import_completed_at_ms 不存在
```

允许例外：

```text
历史 0001/0002/0003
migration test fixture
归档文档
```

### Full incident E2E

保留并实现 v0.17：

```text
T-S04-053
T-FINAL-017
T-FINAL-018
T-FINAL-019
```

最终链：

```text
v3 incident-shaped DB
+ metadata v1 ready fact
+ guardian rollout direct parent
+ state no spawn edge

open new binary → migrate v4
→ scanner sees parser mismatch
→ replay metadata from 0
→ parent/root repaired
→ blocked build member resumes
→ manifest complete
→ epoch activate
→ API totals match new epoch
→ browser/dashboard displays new active totals
```

---

# 12. S10：最终清理 Gate

## 12.1 代码搜索必须为 0

除 migration history / raw external schema / archive 外：

```text
src/usage/carry.rs
pub mod carry
LoadedUsagePlanState
UsageStateProof
UsageCheckpoint           # processor dead type；注意不要误判其他正式同名上下文
ScanEvent                 # wrapper
last_full_import_completed_at_ms
```

runtime `app_meta` SQL：

```text
metadata_parser_version
```

必须 0；per-source metadata fact/checkpoint 中该名称继续合法。

## 12.2 parser version literal

生产：

```text
ScanConfig::new(..., 1)
metadata parser current = 1
```

必须 0。

生产当前版本只来自：

```text
METADATA_PARSER_VERSION = 2
```

## 12.3 provenance

当前 runtime 必须识别：

```text
session_meta_parent
subagent_source
forked_from_id
```

## 12.4 old test standard

active docs 中：

```text
Usagi_测试标准_Spec01-06_v0.15.md
Usagi_测试标准_数据口径改造.md
```

不得继续作为测试来源。

---

# 13. 执行顺序与提交拆分

建议分 6 个逻辑提交，提交顺序即执行顺序。

### C1 metadata parser v2 + direct parent

包含：

```text
METADATA_PARSER_VERSION
ScanConfig version入口删除
payload.parent_thread_id
session_meta_parent provenance
parent conflict
metadata resolver
fixtures/unit tests
```

### C2 runtime 脱离 dead app_meta projections

对应 S4：

```text
Domain 删除两个投影
Storage SELECT/index 删除两个列依赖
Status API 删除 last_full_import_completed_at_ms
Frontend DTO/parser 删除该字段
在仍为 v3 的 DB 上完成局部回归
```

**C2 结束时不要物理删列。**

### C3 SQLite v4 + metadata stale-fact rebuild

严格按：

```text
0004 migration
→ metadata v1→v2 replay
```

包含：

```text
v3→v4 migration
rollback tests
new provenance CHECK
dead app_meta columns 物理删除
metadata v1 ready EOF → v2 full replay
real-shaped DB regression
```

### C4 blocked build recovery E2E

包含：

```text
metadata update → usage reconcile
blocked → rebuilt/carried
manifest completion
activation
API active epoch
Dashboard new totals
```

### C5 其余 reference/dead code deletion

包含：

```text
usage/carry.rs
domain aliases
processor dead structs
ScanEvent
parser1 canonical compatibility
```

### C6 docs + unified test mapping finalization

包含：

```text
v0.17 current-contract 映射最终化
static gate
full regression
```

---

# 14. 验收标准

全部满足才允许验收通过。

## A. Metadata contract

- [ ] `payload.parent_thread_id` 正式解析。
- [ ] Guardian `source.subagent.other` 可识别 Subagent。
- [ ] `session_meta_parent` provenance 独立存在。
- [ ] priority 为 state > direct > nested > fork。
- [ ] 高低优先级 parent 值不一致时 winner 正确且 conflict 不丢。
- [ ] state 无 edge 不覆盖 rollout direct parent。
- [ ] metadata current parser 为 v2 且只有单一代码版本源。

## B. Durable parser upgrade

- [ ] metadata v1 ready fact 在 v2 下不能复用。
- [ ] unchanged/EOF rollout 仍从0重放。
- [ ] migration 不伪造 parser2。
- [ ] new fact/checkpoint 在成功 commit 后为 v2。

## C. SQLite

- [ ] `LATEST_SCHEMA_VERSION=4`。
- [ ] fresh DB → v4。
- [ ] real-shaped v3 DB → v4 无损。
- [ ] `session_meta_parent` 可持久化。
- [ ] app_meta 两 dead 字段从 latest schema 物理删除。
- [ ] rollback 保持完整 v3。
- [ ] 0001/0002/0003 未被修改。

## D. Rebuild / activation

- [ ] 原 blocked Guardian metadata 修复后可重新处理。
- [ ] root_session_id 正确。
- [ ] manifest 未完成时仍禁止 activation。
- [ ] 全完成后 activation 原子切换 active epoch。
- [ ] Query API 使用新 active epoch。
- [ ] Dashboard 不再长期展示旧 epoch。

## E. 冗余清理

- [ ] `src/usage/carry.rs` 删除。
- [ ] production carry 测试覆盖不降低。
- [ ] domain 零调用 aliases 删除。
- [ ] processor 三个 dead structs 删除。
- [ ] `ScanEvent` wrapper 删除。
- [ ] parser1 canonical 新写入兼容删除。
- [ ] global app_meta metadata parser projection 删除。
- [ ] last-full-import dead projection 删除。
- [ ] 没有通过新 alias/fallback 变相保留。

## F. Token canonical 不回退

- [ ] `NormalizedTokenUsage` 仍为 Adapter 后唯一 Token schema。
- [ ] `cache_tokens` 不重新出现。
- [ ] `CacheWriteStatus` 不重新出现。
- [ ] raw Codex Token 字段只在 Adapter/raw fixture。
- [ ] cache-write `None` 不转 0。
- [ ] SQLite v4 不重新引入旧 Token 列。

## G. Tests

- [ ] v0.17 全部 Spec P0/P1 映射到真实存在测试。
- [ ] T-DC 全 PASS。
- [ ] T-S04-053 PASS。
- [ ] T-FINAL-017 PASS。
- [ ] T-FINAL-018 PASS。
- [ ] T-FINAL-019 PASS。
- [ ] 全部 P2 最终 Gate PASS。
- [ ] Rust 全测试 PASS。
- [ ] Frontend unit/typecheck/build/browser PASS。

---

# 15. Luna 完成后必须提交的验收报告

严格按以下格式：

```text
1. 修改文件列表
2. 删除文件列表
3. SQLite user_version / migration 结果
4. METADATA_PARSER_VERSION
5. payload.parent_thread_id raw→fact→root 的实测结果
6. v1 safe fact → v2 replay 的实测结果
7. blocked build → activation 的实测结果
8. 删除的旧 alias / dead struct / reference module 列表
9. app_meta dead 字段 pragma_table_info 结果
10. rg 静态清理 Gate 结果
11. Spec P0/P1 执行结果
12. T-DC 执行结果
13. FINAL 执行结果
14. cargo fmt/test/clippy
15. frontend test/check/build/browser
16. 未完成项
```

`未完成项` 不为 0 时，不得申请最终验收。
