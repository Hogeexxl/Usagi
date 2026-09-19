# Usagi Spec 03：增量扫描器

> 版本：v0.2  
> 状态：当前契约修订版（Spec08 实施目标）  
> 更新日期：2026-08-09  
> 依赖：`Spec_01_数据模型和数据库骨架_v0.2.md`、`Spec_02_Codex原始数据与元数据适配_v0.2.md`  
> 当前唯一测试标准：`Usagi_测试标准_Spec01-06_v0.17.md`  
> 当前版本范围：完整 Token 用量由 Spec 04 接入同一扫描流程；只有 Token 美元费用占位

---

## 1. 目标与边界

本 Spec 实现一个可恢复的增量扫描器：发现 Codex rollout 文件，固定本轮读取视图，只把新增完整行交给 Spec 02，最后原子提交元数据与 metadata checkpoint。

本 Spec 同时完成启动扫描、默认五分钟调度、手动刷新触发、单轮互斥、首次导入、文件移动/截断/替换识别和扫描生命周期。

本 Spec 不实现 Token 解析与 `usage_events`；Spec 04 必须把 usage consumer 接入同一协调流程，使用独立 usage checkpoint。第一版不使用文件监听。

---

## 2. 必须保持的契约

- 只扫描 `$CODEX_HOME/sessions` 与 `$CODEX_HOME/archived_sessions` 下的 `rollout-*.jsonl`；
- Codex 文件始终只读，不读取用户项目源码；
- 路径不是身份；文件身份由 device、inode 和 MU generation 表示；
- metadata 与 usage checkpoint 互不推进、互不重置 parser version；
- checkpoint 只指向换行结束后的完整记录；
- 文件解析结果与对应 checkpoint 共同提交或共同回滚；
- 单个文件失败不阻止其他文件提交；
- `data_revision` 只反映稳定查询事实，扫描状态使用 `status_revision`；
- 当前完整版本必须继续实施 Spec 04 的 Token 去重、恢复、Turn 补偿和聚合；本 Spec 不能把它们改为未来能力。

---

## 3. 模块与外部 interface

扫描器是深模块。HTTP、CLI 和前端不接触目录枚举、文件身份、guard、offset、事务或重建细节。

外部 interface 只提供：

```text
ScanCoordinator::start(config, ledger, codex_metadata) -> ScanHandle

ScanHandle::request(trigger) -> Result<RequestDisposition, ScanRequestError>
ScanHandle::shutdown() -> Result<()>
```

```text
ScanTrigger = Startup | Scheduled | Manual | SourceChanged | Rebuild

RequestDisposition =
  Started { scan_id, started_status_revision }
  Coalesced { followup_scan_id, enqueued_status_revision }

ScanRequestError =
  SourceChanged | Recovering | ShuttingDown
  | StartCommitFailed { kind: Busy | Internal }
  | EnqueueCommitFailed { kind: Busy | Internal }
```

约束：

- `request` 是异步命令；它只等待“本次因果锚点已可用”，不等待整轮扫描完成；
- `Started` 只能在 `mark_scan_started` 成功提交后返回，`started_status_revision` 是该提交返回的 revision；
- `Coalesced` 指向该请求实际排队的 follow-up，而非当前 active scan。`followup_scan_id/enqueued_status_revision` 来自 Spec 01 持久化单槽；
- 同一进程最多有一轮 scan worker；
- 运行期间的新请求只合并为一次持久化 follow-up scan，不建立无界队列；
- 不公开“注册任意 consumer”的插件 interface；当前流水线是 metadata，Spec 04 在模块内部增加固定 usage 阶段；
- 测试直接通过同一协调 interface 驱动，真实临时目录替代用户的 `$CODEX_HOME`。

建议文件布局：

```text
src/scanner/mod.rs          # 外部 interface 与固定流水线
src/scanner/coordinator.rs  # 互斥、请求合并、调度、生命周期
src/scanner/discovery.rs    # 两个来源区域的安全枚举
src/scanner/identity.rs     # 身份、移动、替换、generation 分类
src/scanner/chunk_reader.rs # 固定视图、guard、完整行
src/scanner/report.rs       # 安全计数和错误码
```

这些可以在不损失职责清晰度时合并；不得把扫描实现堆入 `main.rs`。

---

## 4. 核心数据结构

### 4.1 配置

```text
ScanConfig
  codex_home: absolute path
  interval: Duration       # 默认 300 秒，允许 60～3600 秒
```

当前 metadata parser version **不是运行时配置项**。唯一版本源固定为代码常量：

```text
METADATA_PARSER_VERSION = 2
```

`ScanConfig`、CLI、环境变量和 HTTP 请求都不得覆盖该值。测试需要模拟旧 parser 时，只能在 fixture/SQLite 中 seed 旧 `source_checkpoints.parser_version` / `rollout_metadata_facts.metadata_parser_version`，不能把当前 parser 配成 v1。

启动时验证一次绝对路径和 interval。扫描过程中不读取项目目录，也不接受每次请求覆盖扫描根目录。

### 4.2 发现结果

```text
DiscoverySnapshot
  started_at_ms
  areas:
    sessions: Complete | Unavailable(error_code)
    archived_sessions: Complete | Unavailable(error_code)
  files: Vec<DiscoveredFile>

DiscoveredFile
  path
  source_area
  device_id
  inode
  size
  mtime_ns
  filename_thread_id_candidate
```

`Complete` 表示该区域已完整枚举，允许把未出现的旧来源标为 missing。`Unavailable` 时禁止根据“没看到文件”推导删除或 missing。

### 4.3 单文件计划

```text
FilePlan =
  Skip
  ReadFrom { source_file_id, start_offset, observed_size, resume_state }
  Rebuild { source_file_id, observed_size, reason }
  Reject { path, error_code }
```

```text
resume_state = AwaitOwningMeta | OwningLive { owning_thread_id }
```

非零 metadata offset 只能使用持久化来源绑定恢复为 `OwningLive`。不能从 chunk 内容重新猜测先前 replay 状态。

### 4.4 扫描报告

`ScanReport` 只保存计数、耗时、ID 和错误码：发现、新增、移动、追加、重建、跳过、成功、失败、读取字节数、提交完整行数。不得包含 JSONL 原文、Prompt、回复或工具正文。

### 4.5 持久化来源 fact

每个成功处理的 rollout 必须产生一条 `rollout_metadata_facts`，内容与 Spec 02 `RolloutThreadFact` 对应，只保存 owning ID、允许字段候选、ownership 区间摘要、continuation、generation、parser version 和 resolved offset。它不保存 JSON、原始行或正文。

未变化文件可以 Skip 正文，但 resolver 必须通过 Spec 01 `load_metadata_scan_state` 批量加载匹配当前 generation/parser/checkpoint/binding/owning ID 的 safe fact。缺失或 stale fact 不能用最终 `threads` 行反推来源候选，必须从 offset 0 重建 metadata fact。

---

## 5. 实施步骤

### 步骤 1：建立协调器和单轮互斥

1. `start` 创建唯一 coordinator event loop/worker；所有 request、worker terminal、follow-up retry 和 shutdown 事件串行进入该 loop，不为每次触发创建独立协调状态。
2. 在启动 Startup scan、timer 和 HTTP/external request 入口前，必须先完成 `recover_scan_lifecycle()`；恢复未完成前 `ScanHandle::request` 固定返回 `Recovering`，协调器真正进入 shutdown 后才返回 `ShuttingDown`，不使用未声明的 `Unavailable` 变体。
3. 恢复完成后，只在无 active、无 queued follow-up 且 binding ready 时提交一次 `Startup`；queued follow-up 优先于新 Startup。
4. 定时器第一次 tick 设为启动后 `config.interval`，而不是立即 tick，避免与启动扫描重复；默认 interval 为 300 秒。
5. Tokio 定时器使用 missed-tick `Skip` 语义；睡眠恢复后只触发一轮，不补跑所有错过周期。
6. 无 active/queued follow-up 时收到请求：按既定 Started 事务执行。
7. running 且无 queued follow-up 时：按既定 reserve 事务返回 follow-up ID；已 queued 时所有请求复用同一 ID/revision。
8. 当前 worker terminal 后保留 queued，随后尝试原子启动 follow-up。Busy 时 row/槽保持 queued，event loop 安排有界退避后继续重试；单轮有界 retry 耗尽也不放弃调度，下一轮 timer/内部 retry 及进程重启恢复都会继续处理。
9. 只有 source changed、shutdown 或非重试 internal 启动错误才持久化 follow-up `start_failed`；Busy 不得转永久失败。
10. shutdown 先停止接收新请求；queued follow-up 持久化 `start_failed(SCANNER_UNAVAILABLE)`，当前 active 在安全文件事务后调用 `mark_scan_failed(..., SCAN_CANCELLED)`，以 `scan_runs.state=failed` 终止并清 active；协议中不存在独立的 cancelled lifecycle 状态。

验收点：任何时刻数据库最多有一个 active scan；连续点击刷新不会并发打开同一文件或并发推进 offset。

### 步骤 1A：统一启动恢复

`recover_scan_lifecycle()` 顺序固定：

1. **旧 active**：若 app_meta 有 active ID，将对应 `scan_runs.running` 行以 `SCAN_INTERRUPTED` 原子转 failed，写 terminal time/revision、last-finished，清 active，设 scan_state=failed。若同时有 queued，保留 queued。
2. **queued follow-up**：若有 queued，使用原预留 ID 调用 `mark_followup_started`，优先于 Startup。Busy 保持 queued 并进入 event-loop retry；source changed 写 `start_failed(SOURCE_CHANGED)`；shutdown 写 `SCANNER_UNAVAILABLE`；非重试 internal 写 `SCAN_START_FAILED`。
3. **Startup**：只有在无 active、无 queued 且 binding ready 时提交。旧 start_failed 是终态，不阻止新 Startup/Manual。

恢复也须维持 scan_runs 和 app_meta 投影一致。任何事务失败不得只修改内存；恢复遇到持续 Busy 时服务保持 unavailable/recovering，不接受可能扩大队列的新请求。

### 步骤 2：写入扫描生命周期

每轮严格执行：

```text
mark_scan_started(scan_id, trigger, started_at_ms)
→ 扫描和分文件提交
→ mark_scan_completed(scan_id, completed_at_ms)
  或 mark_scan_failed(scan_id, error_code, failed_at_ms)
→ 若 follow-up=queued：
    mark_followup_started(reserved_scan_id, started_at_ms)
    → 执行 follow-up
  或 mark_followup_start_failed(reserved_scan_id, error_code)
```

规则：

- started 写入成功后才执行 I/O；
- 过期 scan ID 不能完成或覆盖新 scan；
- completed 仅用于所有必需阶段完成且没有 hard error；
- 单文件或单区域 hard error 不回滚其他已提交文件，但整轮最终记为 failed；
- state/session-index 暂时不可用、区域权限错误、文件身份竞态和存储失败均产生结构化错误码；
- 已换行的 malformed/unknown 记录由 Spec 02 安全跳过，只产生诊断，不单独令整轮失败；
- failed 不修改上次成功完成时间，也不清空已经稳定提交的数据。

### 步骤 3：读取 state 与 session index 元数据视图

1. 每轮通过 Spec 02 state adapter 读取一次只读快照；state 不可用时保留 `Unavailable` 状态，不能推导字段 Clear 或 Main。
2. 每轮扫描都流式全读一次 `session_index.jsonl`，生成本轮完整 `SessionNameSnapshot`。
3. 不跨轮缓存标题候选，也不能用最终 `threads.title` 代替未变化的 session-index 来源事实；否则高优先级 state 标题删除后无法恢复低优先级标题。
4. session index 缺失视为空兼容来源；权限或 I/O 错误视为 hard error。
5. state/session-index 先形成本轮 resolver 输入，不能在尚未取得全部 present rollout safe facts 时独立覆盖同一 Thread。
6. 只有完成全来源合并后才提交 patch：没有 rollout 来源的 Thread 使用 `commit_metadata(checkpoint=None)`；有 rollout 来源的 Thread 必须连同加载或更新后的 safe facts 按 Thread 组提交。
7. 即使 rollout 正文没有变化，state 快照的真实元数据变化仍可通过完整 resolver 增加 `data_revision`。

本轮 snapshot 只在本轮 resolver 完成前驻留内存，不另建配置或缓存文件。

### 步骤 4：安全枚举两个 rollout 区域

按以下顺序分别枚举：

```text
$CODEX_HOME/sessions
$CODEX_HOME/archived_sessions
```

具体规则：

1. 根目录不存在（ENOENT）视为 `Complete` 的空区域；其他打开错误为 `Unavailable`。
2. 递归只进入普通目录，不跟随目录或文件符号链接。
3. 只接收普通文件和严格匹配 `rollout-*.jsonl` 的文件名。
4. 路径词法规范化后必须仍位于对应根目录；不使用路径作为 Thread/Session ID。
5. 从文件名只提取通过 UUID 校验的 Thread ID candidate；无效 candidate 不阻止枚举，但不能作为可信 ID。
6. 对每个文件读取 device、inode、size、mtime_ns；字段读取失败只拒绝该文件。
7. 同一 device+inode 在同轮出现多个路径时视为 alias：优先 `sessions`，再按规范化路径字典序选择一个；其他路径记录 `DUPLICATE_PHYSICAL_ALIAS`，不重复解析。
8. 枚举结果稳定排序：`sessions` 优先、mtime 从新到旧、最后按路径字典序。首次导入因此优先产生近期数据；排序不影响最终元数据合并结果。
9. 只有完整枚举的区域才能在本轮结束时把未见旧来源标为 missing。

扫描器不得打开 rollout 之外的文件，也不得读取 `$CODEX_HOME` 之外的目标。

### 步骤 5：解析来源身份、移动和 generation

先用完整 `DiscoverySnapshot` 与 Spec 01 `source_files` 现状做批量匹配，再写 observation。不能依赖目录遍历顺序逐条抢占路径。

匹配规则按优先级执行：

1. 同 path、同 device+inode：同一来源，保留 `source_file_id` 和 generation。
2. path 改变、device+inode 仍唯一匹配：物理移动，更新同一来源的 `current_path/source_area`，不重置 checkpoint。
3. 同 path、device 或 inode 改变：路径被替换；保留 path slot 的 `source_file_id`，`file_generation + 1`，更新物理身份，清空旧 `source_files.thread_id`、删除旧 rollout metadata fact，并把该来源所有已存在 consumer 标为 `rebuild_required`。
4. 当前 size 小于上次 `observed_size`：截断；generation 加 1，清空旧 Thread binding、删除旧 fact，所有已存在 consumer 重建。
5. 同身份同 size 但 mtime 改变：视为原地改写；generation 加 1，清空旧 Thread binding、删除旧 fact，所有已存在 consumer 重建。
6. 新 path 且无物理匹配：创建来源；generation 从 1 开始。若 device+inode 曾被缺失历史来源占用，则选择能满足唯一约束的下一 generation。
7. 完整区域中本轮未见的旧来源标为 `missing`；不删除 Thread、事件或 checkpoint。
8. `Unavailable` 区域中的旧来源保持原状态，不能标 missing。

同一次 `record_source_observations` 事务必须完成来源移动/替换、generation、observed size/mtime、missing 状态、generation 变化时旧 Thread binding 失效，以及新 metadata checkpoint(offset=0) 的建立。

文件级替换、截断或原地改写影响整个物理内容，因此必须标记该来源所有已存在 consumer 重建；不能只重建 metadata 而让已有 usage offset 继续前进。parser version 变化只重建对应 consumer。

只有纯 rename 可以继承 confirmed `source_files.thread_id`。任何 generation 变化都代表新的内容代次；新代次必须从 state path、文件名与 owning meta 重新确认 owning ID，不能用旧 binding 构造非零 offset 的 `OwningLive`。

### 步骤 6：生成单文件读取计划

对 metadata consumer 依次判断：

| 条件 | 计划 |
|---|---|
| 新来源或 metadata checkpoint=pending | `ReadFrom(0, AwaitOwningMeta)` |
| processing_status=rebuild_required | `Rebuild`，从 0 开始 |
| parser version 不一致 | 先只标 metadata rebuild，再 `Rebuild` |

当前版本的 metadata parser 为 v2。即使文件 size/mtime/identity 未变化、旧 checkpoint 已在 EOF，只要 durable parser version 仍为 v1，就必须从 offset 0 重放；禁止只把版本号改成 2 或继续复用旧 safe fact。
| processing_status=error，同 generation、身份有效且 guard 匹配 | 从旧 committed offset 重试到本轮计划边界；允许 size/mtime 已增长 |
| processing_status=error，但 generation/身份/guard 任一不可信 | 从 0 重建；文件级不可信时重建所有已有 consumer |
| ready、offset=observed_size、size/mtime/身份未变，且 Ledger 返回 safe fact `Matching` | `Skip`，从数据库加载 fact |
| checkpoint 可用但 safe fact 缺失或 stale | 从 0 重建 metadata fact；不能从 `threads` 反推 |
| ready、offset<observed_size、guard 匹配 | 从 offset 增量读取 |
| offset>observed_size | 所有 consumer rebuild |
| guard 不匹配 | 所有 consumer rebuild |
| 来源绑定冲突或恢复状态不成立 | `Reject`，不推进 offset |

重要：不能只因 size/mtime 与数据库相同就 Skip。只要 metadata checkpoint 小于 `observed_size`，仍有未提交字节或半行，必须重新打开并检查。这保证进程在“写入 observation 后、提交解析前”退出时不会漏读。

非零 offset 的 `resume_state` 规则：

- `source_files.thread_id` 已 confirmed；
- checkpoint 状态为 ready，或为经过下列验证的 error；
- guard 校验通过；
- safe fact 的 generation、metadata parser version、resolved offset 与 checkpoint 一致，continuation 为 `owning_live`，且 `fact.owning_thread_id = source_files.thread_id = OwningLive.owning_thread_id`；
- 上一次提交只允许在 Spec 02 解析器到达稳定 `OwningLive` continuation 后发生。

任一条件不成立必须从 0 重建，不能把非零 chunk 默认视为 owning。

`processing_status=error` 仅在以下条件全部成立时可非零续读：`committed_offset>0`、confirmed binding 存在、同 generation 和身份、guard 匹配、safe fact 与 checkpoint 完全匹配且 continuation 为 `owning_live`。这是对旧成功状态的验证恢复，不要求先把状态改成 ready；新批次成功时在 fact/patch/checkpoint 同事务内回到 ready。offset=0 或缺任一证明时从 0 重建。

### 步骤 7：固定文件视图并消除身份竞态

每个 `ReadFrom/Rebuild` 计划按以下顺序执行：

1. 对发现路径执行 symlink metadata 检查，拒绝 symlink 和非普通文件。
2. 只读打开文件；立即从已打开 handle 读取 device、inode、size 和 mtime。
3. handle 身份必须与计划一致；不一致返回 `SOURCE_CHANGED_BEFORE_READ`，丢弃计划结果。
4. 本轮唯一 `observed_size` 是 discovery/plan 中已经写入 `source_files.observed_size` 的 size。handle size 必须大于或等于该值；若更小，丢弃结果并标记重建。
5. 只读取 `[start_offset, plan.observed_size)`。文件在枚举后、打开前或读取期间追加的字节一律留到下一轮；不能用 handle 的较新 size 扩大本轮边界。
6. 解析后再次 fstat handle。若 size 小于 `plan.observed_size` 或身份异常，丢弃结果并标记重建。
7. 再次 stat 路径。若路径消失或已指向另一身份，当前解析结果不提交，下一轮重新分类。
8. 若路径仍是同一身份但 size 已增长，允许提交 plan 固定视图内结果；新增字节只在下一次 `config.interval` 定时扫描、手动刷新或扫描期间已经由外部触发并合并的 follow-up 请求中处理。文件增长本身不得由当前轮自动设置 follow-up。

任何竞态失败都不能推进 checkpoint。允许记录路径、source ID 和错误码，不记录文件内容。

因此 metadata checkpoint 永远满足：

```text
committed_offset <= 本事务读取的 source_files.observed_size
```

扫描器不得在同一计划中混用“枚举 size”和“打开 handle size”。

这样持续写入的活跃文件也不会形成无间隔自触发扫描循环。

### 步骤 8：实现 guard

guard 使用稳定的 BLAKE3 32-byte digest，窗口固定为 checkpoint 前最多 4096 bytes：

```text
window = [max(0, committed_offset - 4096), committed_offset)
guard_hash = BLAKE3(window)
```

规则：

- offset=0 时 guard 为 NULL；
- 非零 offset 必须有 guard；
- 开始读取前从 handle 重读该窗口并比较；
- 提交新 offset 前基于同一 handle 和新 offset 计算新 guard；
- guard 只校验增量 seam，不用于 Thread ID 或事件 ID；
- hash mismatch 表示可能改写，触发整个来源的所有 consumer 重建；
- guard 算法或窗口改变必须增加相关 parser/scanner 版本并显式重建，不能静默解释旧 hash。

实施时加入直接依赖 `blake3`；不得使用进程随机种子的 `DefaultHasher` 保存持久化 guard。

### 步骤 9：只输出完整 JSONL 行

chunk reader 按字节工作，offset 包含换行符：

1. seek 到 `start_offset`；非零 offset 必须是上一完整行结束位置。
2. 最多读取到固定 `observed_size`，不能调用无界 `read_to_end`。
3. 逐段寻找 `\n`；只有找到换行才向 Spec 02 输出一条记录及 `[line_start, line_end)`。
4. CRLF 输入在交给 JSON parser 前只移除末尾 `\r`；byte offset 仍包含 `\r\n`。
5. 固定视图末尾没有 `\n` 的字节是 half-line：不输出、不持久化，next offset 保持其起点。
6. 单行内存上限为 8 MiB。超过上限后以固定小缓冲继续丢弃到换行；若找到换行，输出安全的 `OVERSIZED_COMPLETE_LINE` 诊断并允许 metadata offset 越过；若未找到换行，仍按 half-line 处理。
7. 空行是完整 malformed 记录，由 Spec 02 产生诊断；不能导致无限重试。
8. reader 返回 `last_complete_offset`、读取字节数和是否存在 half-line。

metadata consumer 可以越过完整的 Ignored、TokenCount、Unknown、Malformed 或 oversized 记录；usage checkpoint 完全不受影响。

### 步骤 10：调用 Spec 02 并维持 continuation

给 Spec 02 的每个请求必须包含：

```text
source_file_id
confirmed/candidate owning_thread_id
source_area
current_path
chunk_start_offset
observed_size
resume_state
完整行迭代器
```

Spec 02 返回本 chunk fact、逐记录 ownership 区间、诊断、最后处理 offset、最终 continuation 和 `needs_rebuild`。scanner 将它与该来源已有 safe fact 合并成新的完整来源 fact：cwd/parent/role 候选连同 provenance 和 record offset 一起保存，先按固定来源优先级合并；parent 固定为 `session_meta_parent > subagent_source > forked_from_id`，同 provenance 保留 offset 最小的第一条可信记录；任意两个非空可信 parent 候选值不一致都标 conflict，高优先级仅决定 winner，不得静默吞掉冲突。增量 chunk 未提供的字段保持已有候选；offset 0 rebuild 则完整替换旧 fact。

提交规则：

- offset=0 从 `AwaitOwningMeta` 开始；
- foreign replay 期间绝不更新 owning Thread；
- 只有解析器最终到达稳定 `OwningLive`，才允许保存非零 metadata checkpoint；
- 到达 EOF 仍为 `AwaitOwningMeta`、`ReplayedAncestor` 或 `UnknownOwnership` 时，不绑定来源、不推进 offset；
- 从非零 offset 恢复时以已 confirmed source binding 构造 `OwningLive`；若新 chunk 出现 foreign `session_meta` 或身份矛盾，立即 `needs_rebuild`，不能继续假定 owning；
- Spec 04 必须复用相同 continuation 规则处理 usage checkpoint，不能把整个 Subagent 文件都计给子 Thread。

更新后的 safe fact 必须标记当前 generation、metadata parser version、最终 offset 和 continuation，并与可选 Thread patch/checkpoint 同事务写入。提交采用 Spec 01 `MetadataThreadCommit`，每个来源显式携带 binding、完整 safe fact 与 checkpoint；resolver 无字段变化时使用 `resolved_patch=None`，不能阻止 offset 前进。解析成功但事务失败时不得单独保存 fact。

为保持 Spec 02 已审核规则，scanner 只负责 continuation 的持久化前提，不重新解释字段优先级或 replay 边界。

### 步骤 11：全轮合并与有限内存

1. 文件正文始终逐行释放；不把完整 JSONL、完整文件或 payload 留在内存。
2. 每个来源最多保留一个安全的 `RolloutThreadFact`、ownership 区间摘要、诊断计数和待提交 offset；Skip 来源从 `rollout_metadata_facts` 加载，不打开正文。
3. 以 owning Thread ID 分组同一 Thread 的普通/归档来源，连同 state/session-index snapshot 一次交给 Spec 02 resolver；同时计算 `ThreadGroupCompleteness`。
4. 一轮中每个 Thread 产生 `None` 或至多一个最终 `ResolvedThreadPatch`；目录顺序不能决定字段值。
5. 父子图和 root 重算使用本轮完整安全 fact 集；父记录晚到时同轮即可解析。
6. 内存复杂度为 O(rollout 文件数 + Thread 数)，与 JSONL 总字节数无关。
7. 文件读取任务最多并发 2 个，数据库 writer 单一串行；此常量第一版不暴露为用户配置。
8. 首次导入可按稳定排序分批读取，但必须在全轮 resolver 完成后再确认全来源 Clear 和关系结论。

`ThreadGroupCompleteness=Complete` 必须同时满足：

- sessions 与 archived_sessions 两个区域均为 `Complete`；
- 该 Thread 本轮发现的每个 present rollout 来源均成功完成身份校验和解析；
- 没有 owning ID、continuation 或同来源冲突；
- replacement/rebuild 来源已经以新 generation 从 0 成功解析；
- resolver 使用的权威来源对准备 Clear 的字段可用。
- 每个 Skip 来源都有与当前 generation/parser/checkpoint 完全匹配的 safe fact。

任一条件不满足时该组为 `Incomplete`。为避免已经推进 checkpoint 后丢失尚未应用的元数据，本 Spec 选择最简单的事务规则：Incomplete Thread 组本轮整体不提交 Thread patch、source binding 或组内 metadata checkpoint。已经成功读取的同组文件下轮从旧 offset 重读；其他 Thread 组继续提交。

Incomplete 组严禁产生 `full_resolution=true`、任何 `Clear`、父/root/role 降级，或根据缺失来源改变 archived/current path 投影。完整区域确认的 missing 来源本身不是读取失败；只有完整来源视图和 resolver 权威来源条件都满足时，它才可参与 Clear 判断。

如果真实历史规模测试显示 fact 集不可接受，必须用测量结果另行设计磁盘 staging；本 Spec 不预建第二套缓存。

### 步骤 12：原子提交与错误隔离

按 owning Thread 分组提交：

```text
confirmed source_files.thread_id bindings
+ 组内每个来源更新或确认的 rollout_metadata_facts
+ 该 Thread 的 None 或至多一个 ResolvedThreadPatch
+ 组内每个来源的 metadata checkpoint/guard/status
+ data_revision（仅事实变化）
= 一个 commit_metadata 事务
```

规则：

- 同一 Thread 的普通和归档来源必须在同一组，避免两个 patch 依赖扫描顺序覆盖；
- 提交前为每个来源传入本轮快照的 `expected_previous_thread_id`：数据库当前值必须与它相等；期望值可为空，非空时必须等于 confirmed owning ID。CAS 通过后先建立/确认 binding，再强制 `MetadataThreadCommit.thread_id = 组内每个 confirmed_owning_thread_id = safe_fact.owning_thread_id = 写入后的 source_files.thread_id`；patch 为 Some 时再要求 `resolved_patch.thread_id` 相等。任一不一致整组回滚；
- 新增完整行只有 TokenCount、Ignored 或其他不改变 Thread 字段的记录时，以 `resolved_patch=None` 提交 binding/fact/checkpoint；`data_revision` 不变；
- `ThreadGroupCompleteness=Incomplete` 时整组不提交，所有组内 checkpoint 保持旧值；
- source binding 冲突时整组不覆盖可信关联，相关 checkpoint 不前进并进入 rebuild/error；
- 一个 Thread 组失败只回滚该组，继续提交其他 Thread 组；
- 无 owning ID 的来源不绑定、不推进，记录安全错误；
- state/session-index 只有在完整 resolver 输入中不存在 rollout 来源时才可 patch-only 提交；有 rollout 来源但 safe fact 缺失、stale 或读取失败时不得独立覆盖该 Thread；
- metadata 提交永远不创建或推进 usage checkpoint；
- 事务成功后才能把内存计划视为完成；fact、patch 或 checkpoint 任一失败时共同回滚，进程从旧 offset 重读；
- 重读依靠规范化 patch 幂等性；Spec 04 依靠确定性 event ID 和唯一约束幂等。

若 replacement/rebuild 需要清除旧 metadata，必须让 Spec 02 在完整来源视图下产生 `full_resolution=true` 的 Clear；扫描器不得直接执行字段级 SQL。

### 步骤 13：missing、归档移动与重复副本

- rename 保持 device+inode 时只更新同一 source 的 path/area，不重建；
- copy 到 archived 产生新物理来源；普通和归档暂时并存时都保留，由 owning Thread 分组和确定性规则合并；
- 完整区域中消失的来源标 missing，但不立即删除 Thread、usage events 或 checkpoint；
- missing 来源再次以同一身份出现时恢复 present；同 path 新身份出现按 replacement 处理；
- `current_rollout_path` 和 archived 投影由 Spec 02 在完整来源视图中解析，scanner 不直接写 Thread 路径；
- 删除历史派生数据或提供“遗忘”设置不属于本 Spec。

### 步骤 14：首次导入与后续扫描

首次导入：

1. 页面可先通过后续 Spec 05 读取空或已有稳定数据；scanner 标记 running。
2. 完整枚举两个区域，按 sessions、近期 mtime、稳定 path 排序。
3. 每个新 metadata checkpoint 从 0 开始；逐行读取，只保留安全 fact。
4. 单文件/Thread 组分事务提交，避免一个覆盖全历史的大事务。
5. 报告进度计数通过 scan state 的后续扩展或日志提供；本 Spec 不把原始路径列表推送前端。
6. 全轮无 hard error 后标 completed；部分失败标 failed，但已成功组保持可查询。

后续启动：

1. 先打开已有 SQLite，旧稳定数据立即可查询；
2. 启动扫描重新读取 state/session index；
3. rollout 只处理 checkpoint 后新增完整行、pending、error 或 rebuild 来源；
4. 已完成且未变化来源不打开正文。

---

## 6. 分类决策表

| 观察 | metadata 动作 | 其他 consumer |
|---|---|---|
| 新文件 | offset 0 导入 | 不创建 usage checkpoint |
| 正常追加 | guard 后增量读取 | Spec 04 各自读取 |
| 未变化且 safe fact 匹配 | Skip 正文，从 DB 加载 fact | 各 consumer 独立判断 |
| 未变化但 safe fact 缺失/stale | metadata 从 0 重建 fact | usage 不变 |
| 仅有未完成 half-line | 重读 tail，不推进 | 不影响 |
| rename 到 archived | 保持 source ID/offset | 保持 |
| copy 到 archived | 新 source | Spec 04 负责事件去重 |
| truncate/replace/same-size rewrite | generation+1，从 0 重建 | 所有已存在 consumer 重建 |
| metadata parser version 变化 | metadata 从 0 重建 | usage 不变 |
| usage parser version 变化 | metadata 不变 | Spec 04 usage 重建 |
| guard mismatch | 来源从 0 重建 | 所有已存在 consumer 重建 |
| region unavailable | 不标 missing | 已有数据保留 |
| 单文件读取失败 | 同 Thread 组整体不提交 | 其他 Thread 组继续 |
| commit 失败 | 整组 offset 回滚 | 其他组继续 |

generation 变化还必须清空旧 owning Thread binding；表中“其他文件继续”指其他 Thread 组。同一 Thread 组因任一 present 来源失败而整体不提交。

---

## 7. 错误码与扫描结果

最低错误码：

```text
SCAN_CANCELLED
SCAN_INTERRUPTED
CODEX_HOME_UNAVAILABLE
SOURCE_AREA_UNAVAILABLE
SOURCE_STAT_FAILED
SOURCE_SYMLINK_REJECTED
SOURCE_IDENTITY_CONFLICT
SOURCE_CHANGED_BEFORE_READ
SOURCE_CHANGED_DURING_READ
CHECKPOINT_OUT_OF_RANGE
CHECKPOINT_GUARD_MISMATCH
OWNING_THREAD_UNCONFIRMED
OWNING_THREAD_CONFLICT
METADATA_CONTINUATION_UNSTABLE
OVERSIZED_COMPLETE_LINE
STORAGE_COMMIT_FAILED
```

错误上下文只允许 scan ID、source ID、规范化 path、offset、consumer kind 和系统/SQLite 错误类别。禁止附带原始 JSONL 行或 payload。

整轮状态：

- `completed`：所有必需区域与计划成功，安全的格式诊断可存在；
- `failed`：任一必需区域、文件身份、读取或提交出现 hard error；已经成功的分组不回滚；
- 未变化完成也必须写 completed，从而增加 `status_revision`，但 `data_revision` 不变。
- 当前 completed/failed 不代表 queued follow-up 完成；队列保留到 follow-up 原子转为 active 或持久化 start_failed。

---

## 8. 与后续 Spec 的契约

### 8.1 Spec 04 Token 账本

Spec 04 必须：

- 在同一来源上创建独立 `consumer_kind=usage` checkpoint；
- 接入同一固定扫描协调器，不能另建第二个目录轮询器；
- 复用 discovery、identity、fixed view、完整行、guard 和 rebuild 决策；
- 复用 Spec 02 ownership continuation，排除 Subagent replay；
- 原子提交 usage events/turns/anomalies、usage checkpoint 和 data revision；
- 独立处理 usage parser version；
- 实现 Token 去重、缺失恢复、Turn 补偿及 Dashboard/Session/模型聚合；
- 只让美元费用保持占位。

### 8.2 Spec 05 查询 API

Spec 05 只调用 `ScanHandle::request(Manual)`，并通过 Spec 01 `scan_status_snapshot(target_scan_id?)` 查询当前投影与持久化 target lifecycle。它不能直接枚举文件、等待整轮 HTTP 请求、修改 checkpoint，或用 `last_finished_*` 代替 target 查询。

---

## 9. 实施顺序

1. 新建 scanner 模块、ScanConfig、trigger/report 类型和 coordinator interface。
2. 接通 started/completed/failed，完成单轮互斥、请求合并、启动扫描和五分钟 timer。
3. 实现两个区域的安全枚举、稳定排序和完整性状态。
4. 在 Spec 01 migration 中加入带 provenance 的 `rollout_metadata_facts`，实现 `load_metadata_scan_state` 批量读取及 generation/parser/offset/binding/owning ID 匹配判定。
5. 扩展 `record_source_observations` 返回分类结果，完成移动、replacement、generation、missing、旧 fact 删除和全 consumer rebuild 标记。
6. 实现计划器；先覆盖崩溃窗口、safe fact 与 checkpoint/observed_size 组合，再接文件读取。
7. 实现只读固定视图、二次身份校验和 BLAKE3 guard。
8. 实现有上限的完整行 reader 和 half-line 行为。
9. 给 Spec 02 请求补入 `resume_state`，要求返回最终 continuation；实现增量 chunk 与已有 safe fact 的确定性合并。
10. 汇总加载/更新后的小型 facts，执行全轮 resolver，按 owning Thread 将 fact、patch、checkpoint 原子提交。
11. 完成 partial failure、外部请求 follow-up、首次导入和重启恢复测试；普通文件增长不得自触发 follow-up。
12. 增加安全计数日志与 README：默认周期、手动触发 seam、无文件监听。
13. 运行格式、单元、集成及真实脱敏 fixture 性能检查。

---

## 10. 测试方案

所有测试使用临时 `$CODEX_HOME`、临时 MU SQLite 和脱敏合成 JSONL；不得读取或修改真实 `~/.codex`。

### 10.1 协调与生命周期

- startup 立即扫描，timer 首次不重复；
- 默认 300 秒与 60～3600 秒验证；
- running 期间多个 manual/timer 请求只产生一个持久化 follow-up，所有 Coalesced 返回同 ID/enqueue revision；
- Started 必须晚于 started 事务提交，返回的 revision 等于该事务 revision；提交失败不返回 Started；
- 当前 active 终态提交后 follow-up 仍 queued；启动事务原子清 queue/设 active，不暴露无队列的中间 idle；
- follow-up started Busy 保持 queued 并由 event loop/重启恢复继续重试；非重试 internal、shutdown、source changed 各自持久化固定 start_failed；
- 前一轮 scan_state=failed、binding ready 且无 queued 时，新 Manual 请求可线性化为 Started；source changed/shutdown/start commit error 返回对应固定错误；
- 文件在读取期间继续增长不会自行产生即时 follow-up；
- missed ticks 不补跑；
- started/completed/failed scan ID 配对；
- 过期 scan ID 被拒绝；
- 无数据变化只增加 status revision；
- shutdown 在文件事务后停止并留下明确状态：active 固定写为 `failed + SCAN_CANCELLED`，queued 固定写为 `start_failed + SCANNER_UNAVAILABLE`；
- 启动恢复覆盖 active-only、active+queued、idle/failed+queued、当前终态后 follow-up started 前崩溃、follow-up started commit 后 I/O 前崩溃、start_failed 提交前崩溃和 Busy retry 期间重启；
- 恢复时旧 active 写 `SCAN_INTERRUPTED`并清 active，queued 优先于 Startup，恢复完成前拒绝新 request；

### 10.2 发现和身份

- 两个区域递归发现；
- 根不存在视为空；权限错误不标 missing；
- symlink、目录、错误扩展名被排除；
- filename UUID candidate 有效/无效；
- rename 保持 source ID、generation、checkpoint；
- copy 创建新来源；
- same-path replacement、truncate、same-size rewrite 增加 generation 并重建所有已有 consumer；
- generation 变化清空旧 Thread binding，纯 rename 才继承；
- 同物理 alias 只处理一次；
- missing 与重新出现；
- 枚举顺序变化不改变结果。

### 10.3 计划与崩溃恢复

- observation 已更新但 parse 未开始时，下轮因 offset<observed_size 继续读取；
- parse 完成但 commit 前退出时，从旧 offset 重读；
- commit 成功后退出时不重复推进；
- offset>size 触发重建；
- parser version 只重建对应 consumer；
- processing error 下轮重试；
- error checkpoint 在文件增长且同 generation/身份/guard 可信时从旧 offset 续读，不可信时重建；
- verified error 只有在 safe fact continuation=owning_live 且 offset>0 时可非零续读；
- 非零 offset 缺 binding、guard 或稳定 continuation 时从 0 重建；
- metadata rebuild 不擅自修改 usage，文件级 rewrite 必须重建所有已有 consumer。

### 10.4 固定视图、guard 与行读取

- 扫描中追加只读到初始 observed size；
- 枚举后、打开前追加仍只读到 discovery plan size，checkpoint 不超过数据库 observed size；
- 路径在 open 前后替换时不提交；
- 读取期间截断不提交；
- 4096 bytes 内 guard 匹配/不匹配；
- offset 0 guard 为 NULL；
- LF、CRLF、空行；
- EOF 完整行推进到 observed size；
- half-line 不输出、不推进；
- half-line 下一轮补全后只输出一次；
- 超过 8 MiB 完整行有界丢弃并推进 metadata；
- 超过 8 MiB 半行不推进；
- 任何测试都不使用无界 read-to-end。

### 10.5 Spec 02 集成

- offset 0 的 main 与 Subagent；
- fork replay 到 owning live 的 continuation；
- 重启后从非零 offset 恢复 OwningLive；
- 增量 chunk 出现 foreign meta 时重建；
- continuation 未稳定时不绑定、不推进；
- 同 Thread 普通/归档 facts 产生 None 或至多一个 patch；
- 重启后 Skip 未变化正文并从 safe fact 恢复完整 resolver 输入；
- safe fact 缺失、generation/parser/offset 不匹配时从 0 重建 metadata fact；
- 增量 chunk 与已有 safe fact 按字段时间规则合并，rebuild 完整替换；
- cwd/parent/role provenance 随值持久化；低优先级追加值不能覆盖已有高优先级值；
- 三类候选的 record offset 随值持久化；同 provenance 的后到冲突不覆盖第一条可信记录；
- fact、source binding、`OwningLive` owning ID 不一致时不能 Skip、续读或提交；
- 每轮重读完整 session index；state 标题删除后仍能恢复 session-index fallback；
- 有 rollout 来源但 fact 不完整时，state/session-index 不独立覆盖该 Thread；
- state 不可用时不清空字段、不错误判定 Main；
- malformed/unknown 完整行不阻塞后续行；
- TokenCount 只让 metadata 越过，不创建 usage checkpoint。

### 10.6 事务和隔离

- source binding、Thread patch、组内 metadata checkpoints 原子提交；
- safe facts、source binding、Thread patch、组内 metadata checkpoints 原子提交；
- 新增区间仅含 TokenCount/Ignored 时，以 patch=None 推进 safe fact/checkpoint，`data_revision` 不变；
- 构造跨 Thread 错误分组，证明 group ID、patch ID 或任一来源 owning ID 不一致都会整组回滚；
- 首次 binding 以 expected None 成功写为 confirmed；陈旧 expected previous 和非空冲突都整组回滚；
- 事务注入失败后所有组内 offset 不变；
- 一个 Thread 组失败，其他组仍成功；
- 同 Thread 任一 present 来源失败时整组不提交，禁止 Clear、关系降级和路径/归档投影变化；
- confirmed ID 冲突不覆盖关联；
- 重复扫描不增加 data revision；
- 多组事实变化各自提交后查询结果正确；
- partial scan 最终为 failed，成功组数据仍可查询；
- 日志和数据库找不到正文哨兵。

### 10.7 性能基线

使用脱敏生成数据记录而非设置硬编码机器耗时门槛：

- 首次导入总字节、文件数、峰值内存和耗时；
- 无变化扫描只发生目录枚举/stat，不打开已完成 rollout 正文；
- 单文件追加读取字节量接近新增区间加 guard 窗口；
- 峰值正文缓冲不超过两个并发 line buffer 加固定开销；
- 报告不输出文件正文。

---

## 11. 独立验收标准

### 11.1 调度

- [ ] 启动立即扫描，默认每 300 秒扫描；
- [ ] timer 的首次及后续间隔使用已验证的 `config.interval`，不是写死常量；
- [ ] 支持手动触发但不并发执行；
- [ ] idle 或 failed 且 binding ready、无 active/queued 时都能发起新扫描；running 只能创建/复用一个 follow-up，source_changed/shutdown 拒绝；
- [ ] Started 返回自身 scan ID/started revision；Coalesced 返回排队 follow-up ID/enqueue revision，不绑定当前 active scan；响应不早于对应 lifecycle/queue commit，也不等待整轮扫描；
- [ ] 运行期间请求最多合并为一次持久化 follow-up；当前终态到 follow-up 启动/启动失败全程可观察；
- [ ] 进程启动先恢复旧 active/queued，queued 优先于 Startup；Busy queued 不会变永久 start_failed 或成为无人处理队列；
- [ ] 普通文件增长不由当前扫描自动触发即时 follow-up，不形成连续循环；
- [ ] 第一版没有文件监听；
- [ ] started/completed/failed 与 status revision 正确；shutdown active scan 使用 failed/SCAN_CANCELLED，不存在额外 cancelled 状态。

### 11.2 增量正确性

- [ ] 每轮固定 observed size，只读到固定边界；
- [ ] discovery plan size 是本轮唯一 observed size；枚举后追加不会使 checkpoint 超过数据库 observed size；
- [ ] 只把换行结束的完整记录交给 adapter；
- [ ] half-line 不解析、不持久化、不越过；
- [ ] 不能因 observation 已写入但 checkpoint 落后而错误 Skip；
- [ ] 正常追加只读取 metadata offset 后新增区间；
- [ ] 未变化来源从匹配的 safe fact 恢复 resolver 输入，不打开正文；fact 缺失或 stale 时从 0 重建；
- [ ] safe fact 必须由同一快照的批量读取接口取得，并同时匹配 binding 与 owning ID；
- [ ] 非零续读只从 confirmed OwningLive continuation 恢复；
- [ ] verified error checkpoint 的非零续读还要求 offset>0 且 safe fact continuation/generation/parser/offset 全匹配；
- [ ] guard、offset 或 continuation 不可信时从 0 重建。

### 11.3 文件生命周期

- [ ] rename 到 archived 保持同一来源和 checkpoint；
- [ ] copy、重复副本和物理 alias 有确定性处理；
- [ ] replacement、truncate、same-size rewrite 增加 generation；
- [ ] generation 变化清空旧 owning Thread binding，只有纯 rename 继承 binding；
- [ ] 文件级 rewrite 将所有已存在 consumer 标为 rebuild；
- [ ] 区域不可用时不误标 missing；
- [ ] 单文件异常不阻塞其他文件。

### 11.4 事务和范围

- [ ] safe facts、confirmed binding、None 或至多一个 Thread patch、组内 metadata checkpoints 原子提交；
- [ ] Thread 字段无变化时允许 patch=None，仍原子推进 safe fact/checkpoint 且不增加 `data_revision`；
- [ ] 每个 source commit 显式携带完整 safe fact；cwd/parent/role provenance 在增量合并后仍正确；
- [ ] Thread group ID、patch ID 与组内所有 binding/fact/continuation owning ID 完全一致；跨 Thread 错误分组整组回滚；
- [ ] 写入前校验 expected previous，允许首次 None→confirmed；完整 ID 等式只在 binding 写入后校验；
- [ ] Thread 组来源不完整时整组不提交，不能产生 full resolution、Clear 或关系/投影降级；
- [ ] 失败或身份竞态绝不推进 checkpoint；
- [ ] metadata 不创建、推进或借用 usage checkpoint；
- [ ] 有 rollout 来源但 safe fact 不完整时，state/session-index patch 不独立覆盖该 Thread；
- [ ] data revision 与 status revision 职责分离；
- [ ] 不保存或输出对话正文；
- [ ] 没有实现 Token 算法，但明确 Spec 04 必须接入同一扫描器完成全部 Token 能力，只有美元费用占位。

### 11.5 工程验证

- [ ] `cargo fmt --check` 通过；
- [ ] `cargo test` 通过；
- [ ] 临时目录集成测试覆盖首次导入、追加、half-line、移动、替换、重建、并发请求和崩溃窗口；
- [ ] 测试不接触真实 `~/.codex`；
- [ ] 无变化扫描不打开已完成 rollout 正文；
- [ ] README 说明默认周期、手动刷新 seam、无文件监听和恢复行为。

---

## 12. 交付物

```text
src/scanner/ 下的协调、发现、身份、读取和报告实现
Spec 02 rollout 请求/结果的 continuation 补充
Ledger `rollout_metadata_facts` migration、来源观察与分组 metadata 提交所需的最小 interface 补充
临时 CODEX_HOME 与脱敏 rollout fixtures
协调、文件生命周期、guard、完整行、恢复和事务集成测试
README 扫描机制说明
```

完成本 Spec 只证明 metadata 扫描链路可增量、可恢复。当前版本仍必须完成 Spec 04 的 usage consumer、Token 账本与 Dashboard/Session/模型聚合。
