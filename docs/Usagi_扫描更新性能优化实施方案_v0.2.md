# Usagi 扫描更新性能优化实施方案 v0.2

> 方案版本：v0.2  
> 日期：2026-08-13  
> 代码基线：`1a8fe45 feat: add usage cost and reasoning effort support`  
> 目标代码目录：`<repo>`  
> 唯一测试标准：`Usagi_扫描更新性能优化测试标准_v0.1.md`

---

## 0. 执行边界

本方案优化启动、自动更新和手动更新中的 **Usage 增量扫描计划加载与执行路径**。

当前真实数据约 440 个 present rollout source / 440 个 owning Thread。已确认主要性能问题是 Usage 阶段反复从 SQLite 恢复全部来源的详细计划状态，而不是 rollout 正文读取、JSON 解析、SQLite 锁等待或写入。

本版目标不是只把“442 次全量详细状态加载”降成“1+n 次全量详细状态加载”，而是直接完成正常稳定轮的增量化：

```text
查看全部 rollout 的基本文件属性
→ 生成本轮 Usage 工作清单 A、B、C
→ 只为 A 加载详细 Usage 状态并处理
→ 只为 B 加载详细 Usage 状态并处理
→ 只为 C 加载详细 Usage 状态并处理
→ 完成
```

正常稳定轮中，不再为未进入工作清单的来源构造完整 `UsageSourcePlan`。

### 0.1 本方案不改

```text
Codex rollout 目录发现规则
metadata parser / resolver / commit 口径
Usage Token / Turn / cost / reasoning effort 口径
Session Main/Subagent/root 聚合语义
active/shadow epoch 的数据可见性语义
rebuild / carry / activation 的证明规则
单批 4 MiB / 4096 lines / 2048 candidates 上限
SQLite schema / migration
HTTP API / SSE / frontend
扫描定时器、手动更新协议
```

### 0.2 本方案明确不做

```text
不同 Thread 并行处理
并行 SQLite writer
metadata / Usage consumer 融合
整文件缓存
新增 background cache / generation map
新增 feature flag、旧路径 fallback 或 dual path
为本次优化增加 migration / 表 / 索引
```

本轮不同 Thread 按稳定顺序 **串行 A → B → C**。并行读取/解析只保留为未来可选优化，不纳入本版实现。

---

# 1. 当前代码事实与新目标

## 1.1 Discovery 已经是正确的轻量入口

`src/scanner/discovery.rs` 当前只做：

```text
目录枚举
symlink / regular file 检查
设备号 + inode
文件大小
mtime
rollout 文件名 Thread candidate
```

Discovery 不打开 rollout 正文。

因此每轮“看一遍 440 个文件”本身可以保留。这里的“看”是读取文件属性，不是读取 1.7GB Session 正文。

## 1.2 当前 Usage 的问题

当前 `run_usage_round()`：

```text
先为全部 present sources 加载完整 UsageScanState
→ 再按 owning Thread 分组
→ 每进入一个 Thread 又为全部 sources 加载一次完整 UsageScanState
→ 最终 activation 前又全量加载一次
```

真实数据库约：

```text
present sources = 440
owning Threads = 440
无 build
```

当前无变化轮约：

```text
442 次全局详细状态恢复
× 每次约 2,201 条小 SQL
≈ 972,842 条小 SQL
```

这一计数描述的是 **SQLite Usage 计划状态恢复成本**，不是整个扫描算法的理论复杂度。

## 1.3 v0.1 方案仍不够彻底

v0.1 的核心是：

```text
整轮持有一份全局 UsageScanState
任何会继续规划的 durable mutation 后，再刷新整份全局 UsageScanState
```

它能把无变化轮从 442 次全量加载降到 1 次，但一个 Thread 单批追加仍会：

```text
全量 440 状态
→ 处理 A
→ 再全量 440 状态
```

n 个 Thread 单批追加会接近：

```text
(1+n) × 440 个详细 source plan
```

v0.2 不采用这个结构。

## 1.4 v0.2 目标

正常稳定轮：

```text
440 个文件属性检查
→ 1 次轻量 Usage 工作清单筛查
→ 工作清单为空：Usage 正文 0 读取，详细 plan 0 加载
```

若只有 A/B/C 三个 Thread 有工作：

```text
440 个文件属性检查
→ 轻量筛查得到 A/B/C
→ 只加载 A 详细状态，处理 A
→ 只加载 B 详细状态，处理 B
→ 只加载 C 详细状态，处理 C
```

正常提交后需要重新确认下一步时，只重新加载**当前 Thread 的来源**，不得重新加载另外 437 个无关来源。

---

# 2. 三类对象必须严格区分

## 2.1 物理 rollout source

一个 Main Thread 或 Subagent Thread 通常对应自己的 rollout 文件；同一 Thread 也可能存在 sessions / archived_sessions 的不同物理来源。

计数和 checkpoint 永远以物理 source 为准，不能把整棵 Session Tree 当成一个文件。

## 2.2 固定 Discovery View

一轮开始后，Discovery 为每个物理 source 冻结：

```text
path
source_area
device_id
inode
size
mtime_ns
```

其中 `size` 是本轮该文件允许读取到的固定上界。

假设：

```text
13:10:00.012  Discovery 看到 B 大小为 10,000,000 bytes
13:10:00.070  真正轮到 B 时，B 已增长到 10,020,000 bytes
```

本轮仍只允许处理到 10,000,000 bytes。新增的 20,000 bytes 留给下一轮。

如果固定边界落在半条 JSON 行中，只提交到上一条完整行结束位置；半行和后续新增内容一起留给下一轮。

## 2.3 Usage 工作清单

新增“轻量 Usage 工作清单”。

它只回答：

```text
本轮哪些 source / Thread 可能需要 Usage 工作？
```

例如：

```text
Thread A -> source 11
Thread B -> source 27
Thread C -> source 91
```

它不为 440 个来源分别恢复完整：

```text
checkpoint object
完整 Usage source state
完整 open Turn object
完整 build proof
最终 PlanAction
```

详细动作仍由现有 `load_source_plan()` 规则决定，但只对工作清单里的少量 source 执行。

---

# 3. Usage 工作清单设计

## 3.1 设计原则：只排除“可证明无需工作”的来源

轻量查询不复制完整 `load_source_plan()` 状态机。

规则是：

```text
能够用廉价证据确定当前 source 是稳定 idle → 不进入工作清单
能够确定当前关系尚不可处理 → 不进入本轮可执行 Thread 清单
其他任何不确定、异常、待恢复、待重建状态 → 进入工作清单
```

因此：

```text
允许极少量 false positive：候选进入详细 planner 后最终得到 Skip/Blocked
禁止 false negative：真实需要处理的 source 不得被工作清单漏掉
```

正常稳定数据库中，无变化来源必须全部满足 idle proof，因此普通热更新不会产生 440 个 false positive。

## 3.2 新增内部返回结构

建议在 storage 层定义最小内部结构，名称可按仓库风格调整，但语义必须等价：

```rust
pub(crate) struct UsageWorkListRow {
    pub source_file_id: i64,
    pub owning_thread_id: String,
}

pub(crate) struct UsageWorkListState {
    pub epoch: UsageEpochState,
    pub rows: Vec<UsageWorkListRow>,
}
```

usage facade 转换成 scanner 可使用的：

```rust
pub(crate) struct UsageWorkList {
    pub epoch: UsageEpochState,
    pub threads: Vec<UsageWorkThread>,
}

pub(crate) struct UsageWorkThread {
    pub thread_id: String,
    pub source_file_ids: Vec<i64>,
}
```

要求：

```text
thread_id 稳定排序
同一 Thread 的 source_file_id 稳定排序、去重
不得把 root Session 作为 Thread 分组键
不得合并不同物理 source 的 checkpoint
```

## 3.3 stable active / 无 build 的工作清单

当：

```text
active_epoch > 0
build_epoch = NULL
working parser = 当前 USAGE_PARSER_VERSION
canonical algorithm 可用
```

只检查本轮 Discovery 中 present 的 source。

一个 source 只有在以下事实全部成立时，才可被轻量查询直接判定为 idle：

```text
1. source 当前 present
2. 已有 owning Thread 且该 Thread 已有 root_session_id
3. Usage checkpoint 存在且 processing_status=ready
4. working epoch 的 usage_source_state 存在
5. source identity/generation 与 state 匹配
6. state parser/canonical 版本与当前版本匹配
7. state resolved offset 与 checkpoint committed offset 匹配
8. state owning_thread_id/root_session_id 与当前 metadata 关系匹配
9. checkpoint guard shape 合法
10. open Turn 的存在性/active_turn_key/thread/generation/state-through 关系可证明一致
11. durable tail 与本轮固定 observed_size 完整匹配：
    - none：checkpoint == observed_size
    - half_line：tail_start == checkpoint < observed_size
```

满足全部条件：

```text
该 source 本轮 Usage 为稳定 idle
不进入详细 planner
```

以下任一情况出现都不能被当作 idle，必须进入候选工作清单：

```text
新 source
文件 generation/identity 变化
文件 size 相比已持久化 tail 变化
无 checkpoint / checkpoint pending / error / rebuild_required
state 缺失或不匹配
parser/canonical/source/root/thread proof 不匹配
guard shape 异常
open Turn proof 不一致
raw tail unverified
此前 ownership 问题已在本轮 metadata 阶段被修复
其他无法证明稳定 Skip 的状态
```

如果 metadata 在本轮结束后仍无法得到 owning Thread/root，则该 source 当前不可执行，不应为了它打开 rollout Usage 正文。它会继续保留 durable 状态，后续 metadata 关系恢复后重新进入工作清单。

## 3.4 build 模式工作清单

当 `build_epoch != NULL` 时，工作清单不能只看文件 size。

必须包含：

```text
当前 build 中 completion_status 仍为 pending / blocked、且存在可执行 Thread 关系的 member
当前 present source 中意外缺少 build membership 的 source（防止 manifest 缺口被静默跳过）
source observation 已使原完成证明失效、重新转为待处理的 member
处于 carry 的 member
```

已经具有仍然有效的：

```text
Rebuilt
Carried
```

完成证明的 member 不进入详细 planner。

如果 build member 当前没有可执行 ownership/root，工作清单可以不把它放入 Thread 执行队列；最终 manifest 完整性检查仍会阻止 activation，因此不得把“没有进入执行队列”解释为“已完成”。

## 3.5 epoch 0 / parser mismatch

轻量工作清单首先读取一次全局 Usage epoch/parser 状态。

如果发现：

```text
active_epoch = 0 且无 build
```

则正常 source 工作清单暂不执行；在 discovery_complete=true 时先启动 shadow build，然后重新生成轻量工作清单。

如果发现：

```text
working parser != USAGE_PARSER_VERSION
```

则仅在**现有状态机原本允许执行该全局 transition 的前置条件全部成立时**，按现有规则 begin / replace build，然后重新生成轻量工作清单。

### 3.5.1 `discovery_complete` 前置条件必须原样保留

本次性能改造只改变“如何找出需要工作的来源”和“详细 plan 的加载范围”，不得删除或放宽当前状态机已经存在的 `discovery_complete` guard。

凡当前代码中原本要求完整 discovery 才允许执行的控制动作，都继续保留原条件，包括其当前实际适用的：

```text
epoch 0 begin rebuild
parser mismatch begin / replace build
RebuildRequired / NeedsRebuild / NeedsRebuildStop / commit requires_rebuild 触发的 build transition
BeginCarry
最终 activation
```

如果某一分支在当前代码中因 `discovery_complete=false` 应 defer / return，则 v0.2 仍按原语义 defer / return；不得为了生成 worklist 而提前执行全局 transition。

不得先为 440 个 source 构造详细 plan，才发现需要全局 rebuild。

## 3.6 SQL 实现约束

工作清单必须是**批量查询**，不能内部再次变成：

```text
for source_id:
  query source
  query thread
  query checkpoint
  query state
  query open turn
```

当前约 440 present sources 时，stable/no-build 路径应由：

```text
1 次 epoch 读取
+ 1 次批量 candidate 查询
```

完成。

若未来 present IDs 超过单条 SQLite bind 参数上限，允许按固定块批量查询；仍必须保持 O(批次数) 的 bulk query，不得退回 per-source N+1。

现有主键/索引足以支持本轮实现。本方案不新增 schema/index；若新版本 profile 证明工作清单 SQL 本身成为新瓶颈，再另立方案。

---

# 4. 详细 Usage plan 改为“按 Thread 精确加载”

## 4.1 不再把完整 `UsageScanState` 当成整轮全局对象

v0.2 不再要求：

```rust
let mut scan: UsageScanState // 包含全部 440 source
```

整轮持有。

改为：

```text
全局只持有：UsageWorkList + 当前 epoch
处理某个 Thread 时：只临时加载该 Thread 的 UsageScanState
```

## 4.2 精确 plan loader

storage 层新增/改造一个“exact source IDs”详细加载入口。

建议语义：

```rust
load_usage_scan_state_exact(
    source_file_ids,
    parser_version,
    expected_epoch,
) -> UsageScanState
```

要求：

```text
只加载调用方明确传入的 source IDs
即使 build 存在，也不得自动把全部 build member 追加进来
仍调用现有 load_source_plan() 作为单 source 最终动作权威
同一次 exact load 使用一个 SQLite read transaction
返回的 epoch 必须与 expected_epoch 一致，否则要求外层重新生成全局工作清单
```

如果不希望新增新名称，也可重构现有内部 loader，但最终生产路径必须只有“worklist bulk query + exact plan load”这一条，不得保留 usage_consumer 中的旧全量 fallback。

## 4.3 为什么 exact loader 仍可保留 per-source 小查询

正常增量轮只对 n 个工作 source 使用 detailed planner。

例如本轮 A/B/C 三个来源有变化，即使每个 detailed source 仍执行约 5 条现有小 SQL，也只是几十条 SQL，而不是对另外 437 个来源重复执行。

因此本版不同时重写 `load_source_plan()` 为复杂大 JOIN，也不引入 statement cache。

先消除错误的工作范围，再根据新 profile 判断剩余热点。

---

# 5. 正常稳定轮的目标执行流程

假设：

```text
440 个来源
A 已完成对话后产生新增数据
B 正在持续对话
C 也产生新增数据
其余 437 个来源无变化
```

## 5.1 固定文件视图

```text
Discovery 查看 440 个文件属性
→ 冻结每个 source 的 identity / size / mtime
```

这一步不读取 rollout 正文。

## 5.2 metadata 先按现有机制完成

保持当前顺序：

```text
source observation
→ metadata 增量读取/解析
→ ownership/root resolution
→ metadata commit
→ Usage
```

这样 Usage 工作清单查询看到的是同一 discovery view 下已经提交的最新 Thread/root 关系。

## 5.3 生成 Usage 工作清单

轻量查询得到：

```text
A
B
C
```

不为其余 437 个来源构造完整 plan。

## 5.4 串行处理 A

```text
只加载 A 所属 Thread 的详细 Usage plan
→ 找到 A 上次 durable checkpoint
→ 打开 A rollout
→ 只读取 checkpoint 到本轮 fixed size 的新增区间
→ 解析 Usage
→ commit
→ 更新 A checkpoint/state
```

如果一次 bounded batch 未处理完 A，本轮后续仍需处理 A：

```text
只重新加载 A 当前 Thread 的详细 plan
→ 从新 checkpoint 继续下一批
```

不得重新生成全局 440-source detailed state。

A 完成后再进入 B。

## 5.5 串行处理 B

如果：

```text
13:10:00.012 Discovery 冻结 B size=10,000,000
13:10:00.070 轮到 B 时真实文件已变成 10,020,000
```

仍然：

```text
只读到 10,000,000
```

后续 20,000 bytes 留给下一轮。

B 不能在本轮不断追逐新的 EOF。

## 5.6 串行处理 C

与 A/B 相同。

## 5.7 完成本轮

若无 build：

```text
bounded cleanup
→ round 完成
→ 等待下一次 scheduled/manual update
```

若有 build：

```text
读取完整 BuildSnapshot manifest
→ 只有全部 member 已 Rebuilt/Carried 才允许 activation
→ CAS activation
→ bounded cleanup
```

activation 前不再加载全部 source 的详细 UsageScanState。

---

# 6. 局部重计划与全局重计划

这是 v0.2 正确性的核心。

## 6.1 局部 durable mutation

只影响当前 source/Thread、不会改变全局 epoch/build membership 范围的操作，后续如果还要继续规划，只刷新当前 Thread。

包括：

```text
普通 commit_group 成功
BeginCarry
ResumeCarry（prefix proof 正常匹配、只推进当前 carry）
CompleteOnly
```

`ResumeCarry` 的 prefix 校验失败不是 local mutation；该路径会触发 build replacement，必须按 6.2 的 global transition 处理。

处理原则：

```text
mutation 成功
→ 如果本轮还需继续该 Thread，exact reload 当前 Thread
→ 不重新读取其他 Thread
```

如果 mutation 后当前 Thread 已明确结束并直接进入下一 Thread，也允许不为一个即将丢弃的旧 plan 做无意义 reload；不得继续使用旧 plan 做下一批即可。

为了降低 Luna 实施错误，本版允许采用更保守的固定规则：

```text
每次本地 durable mutation 成功后 exact reload 当前 Thread
```

即使单批 append 因此多读一次 A 的详细状态，仍只涉及 A，不涉及另外 439 个来源。

## 6.2 全局控制 mutation

会改变全局 epoch/build 或使原工作清单成员范围可能变化的操作，必须丢弃旧工作清单并重新执行轻量工作清单查询。

包括：

```text
首次 active_epoch=0 → begin rebuild
parser mismatch → begin/replace build
ResumeCarry prefix mismatch → replace build sources
RebuildRequired 导致 begin/replace build
NeedsRebuild / NeedsRebuildStop 导致 begin/replace build
commit requires_rebuild 导致 begin/replace build
其他会改变 build manifest 范围或 working epoch/parser 的控制 transition
```

上述 global transition 仍受当前代码既有的 `discovery_complete` 等正确性前置条件约束；本方案只改变 transition 成功后的重计划范围。

固定顺序：

```text
全局 mutation 成功
→ 禁止继续使用旧 worklist
→ load_usage_work_list()
→ 得到新 epoch + 新 Thread 清单
→ 重新进入串行执行
```

这里重新加载的是**轻量工作清单**，不是 440-source 详细 UsageScanState。

## 6.3 terminal mutation

以下操作后本轮不再继续 source planning：

```text
最终 manifest 确认
activate_rebuild
cleanup_inactive
```

因此不要求重新加载 worklist 或 detailed plan。

这取代 v0.1 中过宽的“任何 durable mutation 后必须全量 reload”表述。

## 6.4 cancellation

规则改为：

```text
无 durable mutation → 可立即响应 cancellation
local/global mutation 已成功，但 cancellation 将立即结束本轮 → 可以直接结束，不必为即将丢弃的内存计划额外 reload
mutation 后如果本轮还要继续规划 → 必须先完成对应的 local exact reload 或 global worklist reload，才能继续使用计划
```

禁止：

```text
数据库已发生全局 transition
→ 继续拿旧 worklist 处理 B/C
```

---

# 7. Thread 串行处理与分组语义

## 7.1 本版固定串行

工作清单得到：

```text
A
B
C
```

固定执行：

```text
A 完成/退出
→ B 完成/退出
→ C 完成/退出
```

不创建线程池，不并行打开多个 rollout，不并行 SQLite commit。

## 7.2 同一 Thread 多个物理 source

工作清单按 Thread 分组，但：

```text
每个 source 仍有独立 identity/generation/checkpoint
```

现有 `commit_group()` Thread 组原子性不变。

不得因为工作清单按 Thread 展示，就把多个物理 source 合成一个 checkpoint。

## 7.3 group budget 不变

继续使用：

```text
MAX_BATCH_BYTES
MAX_BATCH_LINES
MAX_BATCH_CANDIDATES
group_budget_allows()
group_budget_full()
dto_requires_exclusive_batch()
```

一个变化 source 如果被预算拆成 B 个 batch，允许同一物理文件进行 B 次**有界增量读取**。

正确目标不是“变化文件整轮绝对只 open/read 一次”，而是：

```text
已 durable commit 的正文区间不得被无意义重复扫描
每批只读取自己的未提交增量区间
未变化 source 正文读取 0
```

---

# 8. 错误隔离

## 8.1 普通 Thread 组错误

在尚未发生需要全局 replan 的 mutation 时，普通单组错误继续保持：

```text
记录 failed_source / error_code
保留 first_group_error
跳过当前 Thread
继续下一个 Thread
```

不得因为本次性能改造把全部 Thread 变成一个大事务。

## 8.2 post-mutation reload 失败

如果 durable mutation 成功，并且本轮还必须继续该 Thread / 全局规划，但紧随其后的必要 reload 失败：

```text
local exact reload 失败
或
global worklist reload 失败
```

必须终止整个 Usage round。

原因不是“全局 440-source snapshot 一定 stale”，而是：

```text
数据库已变化，程序无法取得继续规划所需的权威状态
```

固定错误码建议：

```text
USAGE_PLAN_RELOAD_FAILED        // 当前 Thread exact reload
USAGE_WORKLIST_RELOAD_FAILED    // 全局 worklist reload
```

如果实施员能在不改变现有错误外部契约的前提下复用已有固定码，可复用；测试标准以最终确定的固定码为准，不允许动态字符串。

## 8.3 不存在的 CAS 测试 seam 不得被假设存在

当前仓库的 stale-binding 注入 seam 属于 metadata 测试，不是 Usage `commit_group()` 前的 CAS failpoint。

本方案不要求新增生产 failpoint/mock trait/public seam 来人为制造 Usage CAS 冲突。

相关测试按独立测试标准中的“现有可达普通组错误 + reload fatal 静态/私有 helper 验证”执行。

---

# 9. Storage 层修改

目标文件：`src/storage/usage.rs`

## 9.1 新增轻量 worklist loader

新增内部入口，建议：

```rust
pub(crate) fn load_usage_work_list(
    &self,
    present_source_ids: &[i64],
    parser_version: i64,
) -> StorageResult<UsageWorkListState>
```

职责只有：

```text
读取 epoch/parser
根据 stable/build 模式批量筛选 candidate source IDs + owning Thread
稳定排序
返回
```

不得：

```text
调用 load_source_plan() 遍历全部 present sources
读取完整 open Turn object
把完整 UsageSourceStateWrite 暴露给 scanner
打开 rollout 文件
修改数据库
```

## 9.2 新增 exact detailed loader

建议：

```rust
pub(crate) fn load_usage_scan_state_exact(
    &self,
    source_file_ids: &[i64],
    parser_version: i64,
    expected_epoch: UsageEpochState,
) -> StorageResult<UsageScanState>
```

要求：

```text
source IDs 必须唯一、正数
只加载这些 source
不得自动 union 全部 usage_build_sources
一次 Deferred read transaction
先读当前 epoch，并与 expected_epoch 比较
一致才逐 source 调用现有 load_source_plan()
```

若现有 `load_usage_scan_state()` 只剩测试引用，应重构/删除旧的“build 时自动扩展全部 source”生产语义，禁止 usage_consumer 保留旧 loader 作为 fallback。

## 9.3 保留现有 plan 状态机

以下逻辑不得复制到 scanner：

```text
matching_state
open_turn_internally_matches
durable_tail_matches_source
durable_tail_matches_build
local_replay_safe
BeginCarry / ResumeCarry / CompleteOnly / RebuildRequired 判定
```

最终 detailed action 仍由 storage `load_source_plan()` 决定。

---

# 10. Usage facade 修改

目标文件：`src/usage/ledger.rs`

新增：

```text
UsageWorkList
UsageWorkThread
load_work_list(...)
load_scan_state_exact(...)
```

职责：

```text
把 storage worklist rows 转为 Thread 分组
转换 storage detailed plan 到现有 UsageSourceScanPlan
不新增第二套 planner
```

`pipeline_plan()` 继续接收当前 Thread 的 `UsageScanState` 即可；它不要求该 state 包含全局全部 sources。

旧 `load_scan_state()` 如果不再有生产调用，应按实际引用清理，不保留“旧全量路径 + 新增量路径”双实现。

---

# 11. Scanner Usage consumer 修改

目标文件：`src/scanner/usage_consumer.rs`

## 11.1 `run_usage_round()` 新主循环

目标结构：

```rust
let present = fixed_discovery_present_map(...);
let present_ids = ...;

let mut worklist = usage.load_work_list(&present_ids, USAGE_PARSER_VERSION)?;

'global_plan: loop {
    if discovery_complete && epoch_zero_requires_build(&worklist) {
        begin_rebuild(...)?;
        worklist = reload_worklist_or_fail(...)?;
        continue 'global_plan;
    }

    if discovery_complete && parser_mismatch(&worklist) {
        begin_or_replace_build(...)?;
        worklist = reload_worklist_or_fail(...)?;
        continue 'global_plan;
    }

    for work_thread in worklist.threads.clone() {
        match process_thread_group_exact(..., &worklist.epoch, &work_thread, ...) {
            Completed => {}
            GlobalPlanChanged => {
                worklist = reload_worklist_or_fail(...)?;
                continue 'global_plan;
            }
            OrdinaryError(code) => {
                record_group_error(code);
                continue;
            }
            FatalReloadError(code) => return Err(code),
        }
    }
    break;
}

final_build_manifest_check_and_activation(...)?; // 内部继续保留既有 discovery_complete + manifest + CAS 条件
cleanup(...)?;
```

名称允许不同，控制流语义不得改变。伪代码中未展开的 `RebuildRequired`、`NeedsRebuild*`、carry 等分支也必须继续保留当前代码原有的 `discovery_complete` 前置条件；不得把本图理解为放宽状态机 guard。

## 11.2 `process_thread_group()`

不再接收 `present_ids` 以便每次全量扫描计划；它只接收：

```text
当前 work_thread.thread_id
当前 work_thread.source_file_ids
固定 present map
expected epoch
```

每次进入 group loop：

```text
exact load 当前 Thread source IDs
→ 得到 group detailed plans
→ 执行 control / read / commit
```

本地 mutation 后：

```text
只 exact reload 当前 Thread
```

全局 mutation 后：

```text
返回 GlobalPlanChanged
```

由外层重新生成 worklist。

## 11.3 不允许的旧代码形态

实施完成后，`usage_consumer.rs` 不得再存在：

```text
load_scan_state(&present_ids, ...)
对每个 Thread 重新加载全部 source
activation 前加载全部 source detailed plans
```

静态检查必须能直接证明这一点。

---

# 12. ScanReport 可观察面

目标文件：`src/scanner/report.rs`

新增内部、privacy-safe 性能字段：

```rust
pub(crate) usage_worklist_loads: u64,
pub(crate) usage_worklist_candidates: u64,
pub(crate) usage_detail_plan_loads: u64,
pub(crate) usage_detail_sources_loaded: u64,
pub(crate) usage_global_replans: u64,
pub(crate) usage_worklist_duration_ms: u64,
pub(crate) usage_detail_plan_duration_ms: u64,
pub(crate) usage_detail_source_ids: Vec<i64>,
```

允许按现有命名风格微调。

要求：

```text
仅记录数字、source_file_id、固定错误码
finish() 时 detail source IDs 排序去重
不持久化 SQLite
不加入 API/SSE
不记录 path/JSON/用户内容
不新增 logging dependency
```

这些字段是确定性性能测试观察面，不用不稳定墙钟测试代替。

---

# 13. Durable mutation / replan 矩阵

| 场景 | 数据库动作 | 后续若继续规划 | 范围 |
|---|---|---|---|
| stable Skip / Blocked | 无 | 不 reload | 无 |
| AwaitingOwnership | 无 | 不 reload | 无 |
| 普通 `commit_group` 成功 | checkpoint/state/events/turn/build proof | reload 当前 Thread | local |
| BeginCarry | 初始化 carry | reload 当前 Thread | local |
| ResumeCarry（prefix proof 匹配） | carry page/finalize | reload 当前 Thread（若本轮继续） | local |
| ResumeCarry prefix mismatch | replace build sources | 废弃旧 worklist，重载轻量 worklist | global |
| CompleteOnly | build member complete | reload 当前 Thread（若本轮继续） | local |
| epoch 0 begin rebuild | 创建 shadow build | 重载 worklist | global |
| parser mismatch begin/replace build | 更新 build/parser | 重载 worklist | global |
| RebuildRequired | begin/replace build | 重载 worklist | global |
| NeedsRebuild | begin/replace build | 重载 worklist；原 BuildFrom 仍按现有 stop 规则 | global |
| NeedsRebuildStop | begin/replace build | 本轮 stale fixed view 不重读；若继续其他 source 先重载 worklist | global |
| commit requires_rebuild | begin/replace build | 重载 worklist | global |
| 最终 manifest 确认 | 读取/幂等确认 | 不再 source planning | terminal |
| activate_rebuild | epoch CAS | 不 reload | terminal |
| cleanup_inactive | bounded delete | 不 reload | terminal |

关键定义：

> 只有**后续仍要继续 source planning**时，mutation 后才要求取得相应范围的新权威状态。terminal 操作和因 cancellation 立即结束的 round 不做无意义 reload。
>
> 表中的 global/local 分类只描述“transition 成功后重计划什么范围”；所有 transition 是否允许执行，仍以当前代码既有的 `discovery_complete` 等 correctness guard 为准。

---

# 14. 首次启动 / rebuild 的行为

## 14.1 首次启动

```text
Discovery 查看所有 rollout 属性
→ source observation + metadata 全量建立关系
→ 轻量 worklist 先看到 active_epoch=0
→ begin shadow build
→ 重新生成 worklist
→ worklist 返回全部需要 build 的 Thread
→ 串行逐 Thread exact load + 正文读取 + commit
→ manifest 全完成
→ activation
```

首次导入本来就是全量正文工作，但不再产生“每处理一个 Thread 又详细恢复全部 440 source”的额外成本。

## 14.2 parser mismatch / shadow rebuild

同理：

```text
先做全局 transition
→ 轻量 worklist 得到 pending build members
→ 逐 Thread exact plan
```

即使 build 包含 440 个 member，也只是每个实际待处理 member 在轮到自己时加载详细状态，不会形成 `440 × 440` 的计划恢复。

---

# 15. 正常稳定增量的性能模型

这里描述的是 **Usage planning / state load**，不声称整个扫描程序所有 Rust 操作都变成同一复杂度。

定义：

```text
N = 当前 discovery present sources（约 440）
W = 本轮工作清单 candidate sources
G_w = 本轮工作 Thread 数
B = 工作 source 因 bounded batch 产生的额外本地重计划次数
R = 本轮全局 rebuild/control transition 次数
```

正常无 build、无全局 transition：

```text
worklist bulk scan：O(N)，但只有常数级 SQL
详细 plan：O(W + B) 个 source 级恢复
rollout 正文：只读取 W 中实际需要 read 的增量区间
```

无变化：

```text
W=0
详细 plan=0
正文=0
```

A/B/C 单批追加：

```text
W≈3
只对 A/B/C detailed load/reload
其余约 437 个来源不进入 detailed planner
```

发生 global transition 时：

```text
重新执行轻量 worklist
```

不是重新执行全量 detailed plan。

---

# 16. 预期文件修改清单

## 16.1 生产文件

预期修改：

```text
src/storage/usage.rs
src/usage/ledger.rs
src/scanner/usage_consumer.rs
src/scanner/report.rs
```

## 16.2 测试文件

按独立测试标准，允许修改：

```text
src/storage/usage.rs 的现有 private tests / 其 tests 子文件
src/scanner/mod.rs 的 private tests
必要时新增 src/storage/usage/tests/usage_incremental_scan.rs
```

## 16.3 正常不应修改

```text
src/scanner/discovery.rs
src/scanner/chunk_reader.rs
src/scanner/pipeline.rs
src/storage/source.rs
src/usage/pipeline.rs
src/usage/processor.rs
src/api.rs
src/api/query.rs
src/storage/schema/*
src/storage/migrations.rs
frontend/**
Cargo.toml
Cargo.lock
```

如果施工发现必须修改上述文件，必须先说明具体 correctness 阻塞；不得为了方便扩大范围。

---

# 17. Luna 施工 + 测试 Gate 总图

本章是执行顺序，不复制正式测试条目。正式断言以 `Usagi_扫描更新性能优化测试标准_v0.1.md` 为唯一标准。

## 17.1 并行原则

所有并行 Track 必须满足：

```text
同一时间不修改同一个文件
公共类型/函数签名先冻结
每个 Gate 合并后再统一 cargo fmt
不得让两个施工员各自“顺手修”同一公共模块
```

## 17.2 Gate 0：契约冻结，不改行为

### 文件所有权总表

| Track | 允许修改文件 | 对应正式测试 | 是否可与同波其他 Track 并行 |
|---|---|---|---|
| A | `src/storage/usage.rs`、其专属新 storage test 子文件 | T-PERF-001～003 中 storage 部分 | 是 |
| B | `src/usage/ledger.rs` | T-PERF-003 facade 部分 | 是 |
| C | `src/scanner/report.rs` | Gate B 的 report 确定性计数 | 是 |
| D | `src/scanner/usage_consumer.rs` | T-PERF-004～008 的生产路径 | 与 E 并行 |
| E | `src/scanner/mod.rs`、其专属 scanner test fixture | T-PERF-004～008 测试代码 | 与 D 并行 |

同一 Gate 内任何文件只有一个 Track 拥有写权限。发现必须跨 Track 修改时，先在 Gate 合并点交接文件所有权，不允许两个 Track 同时改同一文件后再人工拼冲突。

先确认并锁定以下内部契约：

```text
UsageWorkListState / UsageWorkListRow
UsageWorkList / UsageWorkThread
load_usage_work_list(...)
load_usage_scan_state_exact(...)
ScanReport 新计数名称
GlobalPlanChanged / fatal reload 的控制流语义
```

Gate 0 只确认签名和责任边界，不允许先写第二套临时 API。

## 17.3 第一并行波：Storage / Facade / Report

```text
                         ┌─ Track A：Storage worklist + exact loader
Gate 0 契约冻结 ────────┼─ Track B：Usage facade 类型/转换
                         └─ Track C：ScanReport 观察面
```

### Track A

只修改：

```text
src/storage/usage.rs
必要的新 storage private test file
```

完成：

```text
轻量 worklist bulk query
stable idle proof
build candidate 规则
exact detailed loader
旧 build 自动扩展路径的生产清理
storage 定向测试
```

### Track B

只修改：

```text
src/usage/ledger.rs
```

完成：

```text
worklist facade types
Thread 分组/稳定排序
exact detailed plan 转换
不复制 planner 逻辑
```

### Track C

只修改：

```text
src/scanner/report.rs
```

完成：

```text
worklist/detail/global replan counters + duration
finish() 字段使用
privacy-safe
```

三个 Track 可以并行，因为不修改同一文件。

### Gate A 合并测试

第一波合并后：

```text
cargo fmt --check
cargo check
运行测试标准 Gate A：T-PERF-001 ～ T-PERF-003
```

Gate A 失败只修 A/B/C 对应文件，不进入 scanner orchestration。

## 17.4 第二并行波：Scanner 主控制流 / Scanner 测试

Gate A 通过后：

```text
                     ┌─ Track D：usage_consumer 新增量控制流
Gate A ──────────────┤
                     └─ Track E：scanner private 测试 fixture / 正式测试
```

### Track D

只修改：

```text
src/scanner/usage_consumer.rs
```

完成：

```text
删除全局 UsageScanState 主循环
接入 worklist
Thread 串行 A→B→C
exact current-Thread plan load
local replan
global worklist replan
固定 discovery view 继续透传
activation 不再全量 detailed load
错误/cancellation 分流
```

### Track E

只修改：

```text
src/scanner/mod.rs
必要时 scanner 当前 tests 可引用的新 fixture 文件
```

完成：

```text
无变化 round
n Thread 增量 round
持续增长 B 的 fixed-view 场景
report 确定性计数
普通组错误隔离
```

Track D/E 可以并行；E 按 Gate 0 已冻结 API/计数编写测试，不修改 `usage_consumer.rs`。

### Gate B 合并测试

```text
cargo fmt --check
cargo check
运行测试标准 Gate B：T-PERF-004 ～ T-PERF-006
```

必须先证明正常稳定增量路径成立，才能进入 rebuild/carry 最终回归。

## 17.5 Gate C：控制状态与受影响回归

Gate B 通过后，不再开新的并行生产改造；只修复失败点。

执行：

```text
T-PERF-007（含 ResumeCarry prefix mismatch 的 global replan 边界）
T-PERF-008
现有 Spec04 scanner/rebuild/carry/multi-batch/guard/half-line 受影响回归
```

并执行静态检查：

```bash
rg -n "load_scan_state\(&present_ids|load_scan_state\(present_ids" src/scanner/usage_consumer.rs
rg -n "load_usage_scan_state" src/scanner/usage_consumer.rs src/usage/ledger.rs
rg -n "load_usage_work_list|load_usage_scan_state_exact" src/scanner src/usage src/storage
```

必须确认生产 Usage consumer 不存在旧全量 fallback。

## 17.6 Gate D：最终完整回归 + 真实数据性能

只有 Gate C 全部通过后执行：

```text
测试标准 Gate D 全部命令
真实 CODEX_HOME 三轮无变化 release 性能验收
```

不得在 sampler/profiler 开启时采集硬门槛时间。

若确定性计数正确但墙钟仍不达标：

```text
停止扩大实现
重新 profile 新版本
提交新最高耗时栈
```

不得自动继续做 statement cache、并行解析、schema/index 或 metadata/Usage 融合。

---

# 18. 禁止实现清单

以下任一项出现即退回：

```text
1. 正常稳定轮先加载全部 present sources 的完整 UsageScanState
2. 每处理一个 Thread 都重新加载全部 source detailed plan
3. normal commit 后刷新全局 worklist 或全局 detailed state
4. build 存在时 exact loader 自动扩展全部 build member
5. 仅按 mtime/size 判断工作清单，忽略 durable pending/rebuild/carry/ownership 状态
6. worklist 为了“快”而允许 false negative
7. 用手工猜测 SQLite mutation 结果跨多个 batch 继续使用旧 plan
8. B 在本轮追逐 discovery 之后继续增长的 EOF
9. 不同 Thread 并行处理或并行 SQLite writer
10. 修改 Token/cost/reasoning effort 数据口径
11. 修改 active/shadow epoch、carry、activation 证明
12. 修改 batch 上限
13. 新增 migration / 表 / 索引
14. 新增第三方依赖
15. 新增 feature flag / fallback / dual path
16. 删除、skip、ignore、放宽既有测试
17. 为测试新增生产 CAS failpoint/mock trait/public seam
18. 用 sleep 或宽松墙钟断言代替确定性工作范围测试
```

---

# 19. 最终完成判定

只有以下全部满足才可报告完成：

- [ ] 正常稳定轮不再构造全局完整 `UsageScanState`；
- [ ] worklist 使用 bulk query，当前约 440 sources 不存在 per-source N+1 candidate 查询；
- [ ] 无变化轮 worklist 为空、detailed plan 0、Usage 正文 0；
- [ ] n 个变化 Thread 只为工作清单中的 Thread 加载 detailed plan；
- [ ] normal local mutation 后最多只 replan 当前 Thread，不触碰无关 Thread；
- [ ] global control mutation 后废弃旧 worklist，并只重新生成轻量 worklist；
- [ ] exact loader 在 build 模式也不会自动扩展全部 build members；
- [ ] 当前状态机既有的 `discovery_complete` guard 未被删除或放宽；
- [ ] ResumeCarry prefix mismatch 按 global transition 废弃旧 worklist；
- [ ] Thread 固定串行，不新增并发；
- [ ] fixed discovery size 仍是本轮读取硬上界；
- [ ] multi-batch 只重复读取尚未 durable commit 的有界增量，不承诺“变化文件绝对只 open 一次”；
- [ ] activation 仍由完整 BuildSnapshot manifest + CAS 证明；
- [ ] 普通组错误隔离保持；必要 post-mutation reload 失败按 fatal 规则处理；
- [ ] ScanReport 提供 worklist/detail/replan 的内部确定性计数；
- [ ] 独立测试标准全部 PASS；
- [ ] 真实约 440 sources 无变化 release 更新达到测试标准性能硬门槛；
- [ ] 无 schema/API/frontend/依赖/无关重构；
- [ ] 不存在旧全量 Usage consumer fallback。

完成后的正常稳定轮预期：

```text
440 个文件：只做轻量属性发现
SQLite：1 次轻量 Usage worklist bulk 筛查
无变化：0 个 detailed plan，0 个 rollout Usage 正文读取
A/B/C 变化：只 detailed plan + 读取/提交 A/B/C
全局 rebuild：才扩展到所有真正 pending build member
```
