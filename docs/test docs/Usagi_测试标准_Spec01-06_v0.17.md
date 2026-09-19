# Usagi 测试标准（Spec 01～06）v0.17

> 本文是当前版本 Spec01～06 的唯一统一测试标准。v0.17 **取代** v0.16/v0.15，并已吸收原 `Usagi_测试标准_数据口径改造.md` 的有效要求。功能语义以 Spec01～06 v0.2、两份数据口径和 Spec08 当前实施目标为准；Spec07 是已完成历史实施方案，不再作为当前测试来源。
>
> 本文只定义“必须验证什么”，**不继承历史 PASS 状态**。测试执行结果必须另写执行记录；代码或 Spec 发生语义变化后，旧 PASS 不得自动视为新版本 PASS。

## 0.1 本标准的作用边界

本文是 **统一回归/验收标准**，不是新的 Spec01～06 开发计划。

当前代码已经完成 Spec01～06 以及 Spec07 的既有实现。本轮只实施 Spec08 delta。

因此：

```text
本文覆盖 Spec01～06
≠
本轮重新实现 Spec01～06
```

Spec01～06 的测试条目用于证明 Spec08 修改没有破坏已有功能。只有 Spec08 明确涉及的代码和因 Spec08 引发的真实回归才进入本轮修改范围。

不得因为本文列出了某个历史/既有测试，就恢复已经废弃的旧代码入口；如果测试本身基于旧契约，应该按当前 Spec/数据口径修订测试。

---

## 1. 当前基线与强制原则

- Spec02 基线：`Spec_02_Codex原始数据与元数据适配_v0.2.md`。
- Token canonical：`normalizedTokenUsage数据口径.md` v0.2、`codex rollout数据口径.md` v0.2。
- Usage parser/canonical：v3。
- Metadata parser：v2；当前版本只能来自代码常量 `METADATA_PARSER_VERSION`，不得由 `ScanConfig`、CLI、环境变量或 HTTP 注入。
- Current latest SQLite schema：v4（0001 metadata → 0002 usage ledger → 0003 normalized token → 0004 metadata parent v2/current-schema cleanup）。
- Current `app_meta` 不存在 `metadata_parser_version`、`last_full_import_completed_at_ms`。
- Production carry 只有 `src/storage/usage.rs` 一套持久化实现；`src/usage/carry.rs` reference state machine 必须删除。
- 当前实际 Provider/source：OpenAI Codex rollout；不要求 Responses/Chat Completions/Anthropic/Gemini Adapter。
- 测试不得访问或修改真实 `~/.codex`；真实样本必须复制并脱敏为 fixture。
- 真实 Codex schema 契约项不得只用“测试自创 JSON”验证，必须有结构等价于真实 rollout 的 fixture。
- 不允许为了旧 Spec/旧测试过测而保留 runtime fallback、旧 Token 字段、旧 provenance alias 或 dual-read/dual-write。

### 1.1 真实 rollout 必备 fixture

至少保留三类结构：

```text
A. Main
   payload.parent_thread_id absent
   payload.forked_from_id absent
   payload.source = string/main source

B. Legacy Subagent
   payload.parent_thread_id present
   payload.forked_from_id present
   payload.source.subagent.thread_spawn.parent_thread_id present

C. Guardian/other Subagent
   payload.parent_thread_id present
   payload.forked_from_id absent
   payload.source.subagent.other present
   nested thread_spawn.parent_thread_id absent
   state_5.thread_spawn_edges child row absent
```

三类 fixture 只保留结构、UUID 关系、必要时间/model/token 元数据；Prompt/回复/reasoning/tool 正文统一替换 sentinel。

## 1.2 当前唯一文档基线

Luna 执行和验收时只使用以下 active 契约：

```text
Spec_01_数据模型和数据库骨架_v0.2.md
Spec_02_Codex原始数据与元数据适配_v0.2.md
Spec_03_增量扫描器_v0.2.md
Spec_04_Token账本与聚合_v0.2.md
Spec_05_查询API与更新通知_v0.2.md
Spec_06_01_前端框架与Dashboard界面_v0.2.md
Spec_06_02_Session记录列表_v0.2.md
normalizedTokenUsage数据口径.md
codex rollout数据口径.md
Spec08_真实Codex适配与旧方案冗余清理实施方案_v0.2.md
```

`Spec_07_数据口径改造实施方案.md` 是已经完成的历史实施方案，只用于解释 v3 Token canonical 的形成过程；不得覆盖 Spec08 的当前目标。v0.15/v0.16 与独立数据口径测试文档均不得作为当前测试来源。

## 2. 优先级与完成门

| 优先级 | 要求 |
|---|---|
| P0 | 数据正确性、事务/迁移原子性、身份/去重、隐私、安全、epoch 激活、API 契约。任一失败阻断。 |
| P1 | 核心兼容、状态机、主要异常恢复、UI 主功能。对应 Gate 必须通过。 |
| P2 | 压力、极端组合、资源预算。最终完整测试必须执行。 |

每个 Spec Gate：执行该 Spec P0/P1 + 所有延期到该 Gate 的前置条目。最终发布门：重新执行全部 P0/P1，并执行全部 P2、T-DC 与 FINAL。

## 3. Spec01 测试条目

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-S01-001 | 独立闭环 | P0 | S01 完成门 | `0001_initial.sql` 隔离 fixture：六张基础 metadata 表、scan_runs 空、user_version=1、revision/active/follow-up 初值与 PRAGMA 正确；明确这是 v1 migration 单层测试，不代表当前 `Ledger::open()` fresh DB 最终版本。当前 fresh-open latest schema 另由 migration/T-DC Gate 验证。 |
| T-S01-002 | 独立闭环 | P0 | S01 完成门 | 来源身份约束：路径唯一、device/inode/generation 唯一约束、非法 size/generation 拒绝。 |
| T-S01-003 | 独立闭环 | P1 | S01 完成门 | 来源移动与副本：rename 保持 source ID；普通/归档副本可独立作为来源并关联同一 Thread。 |
| T-S01-004 | 独立闭环 | P0 | S01 完成门 | generation 变化使旧 Thread binding 失效，并将已存在 consumer 标记为需要重建；纯路径移动不触发该失效。 |
| T-S01-005 | 独立闭环 | P0 | S01 完成门 | safe metadata fact 不含正文；仅 generation/parser/offset 与 source 状态匹配时可作为可复用事实。 |
| T-S01-006 | 独立闭环 | P1 | S01 完成门 | cwd/parent/role 的 provenance 与 record offset 持久化；parent provenance 必须能区分 `session_meta_parent`、`subagent_source`、`forked_from_id`；固定优先级、同 provenance 第一可信记录和 conflict 规则稳定。 |
| T-S01-007 | 独立闭环 | P0 | S01 完成门 | metadata owning-ID/分组/CAS 存储不变量：fact、binding、expected previous、group/patch ID 不一致时拒绝并整组回滚；首次 None→confirmed 合法。 |
| T-S01-008 | 独立闭环 | P0 | S01 完成门 | 批量 metadata scan state 在同一只读快照内返回 source、checkpoint、safe fact 及匹配状态。 |
| T-S01-009 | 独立闭环 | P0 | S01 完成门 | fact + Thread patch + metadata checkpoint 原子提交；约束/注入失败、未提交退出后重开均无部分写入，事务至多增加一次 data_revision。 |
| T-S01-010 | 后置联动：S02/S03 | P0 | S03 完成门 | 真实 parser/scanner 产生仅 TokenCount/Ignored 的新增区间时，以 resolved_patch=None 推进 safe fact/checkpoint 且 data_revision 不变。 |
| T-S01-011 | 独立闭环 | P0 | S01 完成门 | generation 变化删除旧 safe fact。 |
| T-S01-012 | 后置联动：S03 | P1 | S03 完成门 | 真实 scanner 对未变化 rollout 不打开正文，能从匹配 safe fact 恢复 resolver 输入。 |
| T-S01-013 | 独立闭环 | P0 | S01 完成门 | metadata/usage checkpoint 共存且互不推进；offset 不超过 observed size；单 consumer rebuild 不污染另一 consumer；删除 source 级联删除 checkpoints。 |
| T-S01-014 | 独立闭环 | P0 | S01 完成门 | Thread patch 合并语义矩阵：Keep/Set/Clear、resolved_at 防旧覆盖、conflict、unknown 不产 root、多来源只保留一行 Thread。 |
| T-S01-015 | 独立闭环 | P1 | S01 完成门 | 重复批次不产生重复 Thread；幂等提交不多增 revision。 |
| T-S01-016 | 后置联动：S04 | P0 | S04 完成门 | build manifest add/replacement 与 source observation 必须单事务；崩溃后 source、manifest、app_meta build 列、usage checkpoint 只能处于完整前态或完整后态。 |
| T-S01-017 | 后置联动：S04 | P0 | S04 完成门 | rebuilt/carried/pending/blocked/carry-in-progress 的 source observation 与 build disposition 转换、required boundary、tail proof、CompleteOnly/VerifyRawTail 规则形成真实闭环。 |
| T-S01-018 | 后置联动：S04 | P0 | S04 完成门 | build replacement 保留旧 manifest 全集和可信 build-only proof/progress；不可信 present 重建、missing 保持 blocked，旧成员不得消失。 |
| T-S01-019 | 后置联动：S04 | P0 | S04 完成门 | commit_metadata 中 binding/root、safe fact/checkpoint、active usage reconcile、build disposition 同事务；任一步失败全部回滚，首次 None→confirmed 不允许事后补 reconcile。 |
| T-S01-020 | 独立闭环 | P0 | S01 完成门 | 扫描生命周期存储不变量：started/terminal row 与 active/last-finished/status_revision 原子；running/idle/failed/queued CHECK 与 CAS；过期 scan ID 拒绝；失败不清稳定数据。 |
| T-S01-021 | 独立闭环 | P1 | S01 完成门 | 持久化 follow-up 单槽与 target scan 历史：running 时复用同 queued ID/revision；终态后 target row 不被后续 scan 覆盖；无业务变化不增加 data_revision，status_revision 可独立变化。 |
| T-S01-022 | 后置联动：S03 | P0 | S03 完成门 | 协调器真实执行 follow-up Busy/retry、internal/shutdown/source_changed→start_failed 与 active shutdown→SCAN_CANCELLED 的状态迁移。 |
| T-S01-023 | 独立闭环 | P0 | S01 完成门 | CODEX_HOME：首次绑定、相同 fingerprint 正常写、不同 fingerprint 标 source_changed 并拒绝采集写；旧库可读且不混合两个 Home。 |
| T-S01-024 | 独立闭环 | P0 | S01 完成门 | migration：新建库可从 0 迁移到当前 latest schema；历史 v1/v2/v3 数据按现行迁移链原子升级；新增 metadata parent provenance 的持久化约束必须通过正式 migration 演进，禁止篡改历史 migration；SQL 与 user_version 同事务，中途失败共同回滚，高版本拒绝写，重复打开幂等。 |
| T-S01-025 | 独立闭环 | P0 | S01 完成门 | 隐私边界：schema 无正文列；错误日志不含正文 sentinel；测试绝不读取真实 ~/.codex。 |
| T-S01-026 | 独立闭环 | P2 | 最终完整测试 | 范围/文档一致性静态检查：Spec01 不创建 Token 表，整体计划仍包含当前版本 Token Spec。此项不应占用日常运行测试时间。 |
| T-S01-027 | 独立闭环 | P0 | S01 完成门 | current schema v4 契约：历史 `0001` 可隔离复现，但 fresh-open latest 的 `app_meta` 物理不存在 `metadata_parser_version` 与 `last_full_import_completed_at_ms`；`rollout_metadata_facts.parent_hint_provenance` 允许 `session_meta_parent`；v3→v4 迁移无损且失败原子回滚；执行顺序必须先让 runtime 脱离两个 dead app_meta 列，再物理删列，禁止出现 `no such column` 中间态。 |

## 4. Spec02 测试条目

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-S02-001 | 独立闭环 | P0 | S02 完成门 | State adapter 只读白名单：完整/最小 schema、threads 缺失、可选列/spawn 表缺失的降级；禁止列不进入 SQL；只读连接不可写。 |
| T-S02-002 | 独立闭环 | P1 | S02 完成门 | State 时间兼容矩阵：秒/毫秒输入规范化正确。 |
| T-S02-003 | 独立闭环 | P1 | S02 完成门 | Session index：thread_name 解析、按时间 latest-wins、同时间冲突诊断、空标题忽略。 |
| T-S02-004 | 独立闭环 | P1 | S02 完成门 | Session index 流式容错：invalid JSON 不阻断、half-line 不算完整记录、超长行/标题有界拒绝。 |
| T-S02-005 | 独立闭环 | P0 | S02 完成门 | Rollout ownership/真实 session_meta 基础矩阵：main；legacy Subagent（顶层 parent + nested thread_spawn + forked_from）；Guardian/other Subagent（仅 `payload.parent_thread_id` + `source.subagent.other`）；父 session_meta/turn_context replay 不污染 owning Thread cwd/model。 |
| T-S02-006 | 独立闭环 | P0 | S02 完成门 | OwningLive/replay 区间：满足 UUIDv7 边界后恢复 Owning；无法确认则保持 ReplayedAncestor/UnknownOwnership；direct-parent Guardian 形态与 legacy Subagent 形态均保持相同 ownership 状态机；每条记录有确定 ownership 区间与置信状态。 |
| T-S02-007 | 独立闭环 | P0 | S02 完成门 | continuation：offset0 confirmed 且无 foreign replay 可稳定续读；nonzero 仅从 confirmed OwningLive；首次遇到 foreign meta 时本 chunk 零提交并要求从0重建。 |
| T-S02-008 | 独立闭环 | P0 | S02 完成门 | owning ID 校验：filename/owning meta 一致；state path 冲突或 owning meta 缺失进入安全不确定/冲突路径；`payload.parent_thread_id` 只能来自 owning meta，foreign replay meta 的 parent 不得污染 owning Thread。 |
| T-S02-009 | 独立闭环 | P1 | S02 完成门 | Rollout 分类：多 Turn model 正确；TokenCount 仅分类；正文完全忽略；unknown/malformed 只产生安全诊断。 |
| T-S02-010 | 独立闭环 | P0 | S02 完成门 | 关系图正常路径：单 main、无父边 main 判定、一层/多层 Subagent、父记录晚到；parent 优先级固定为 state explicit edge > owning `payload.parent_thread_id` > nested `source.subagent.thread_spawn.parent_thread_id` > 受限 `forked_from_id`；state 无 child edge 时不得否定 rollout direct parent。 |
| T-S02-011 | 独立闭环 | P0 | S02 完成门 | 关系图不确定/冲突：state 不可用无明确证据→unknown；state edge 与 direct parent 冲突、direct/nested/forked 多父冲突、自环/环、role-edge 冲突均安全降级并产生稳定诊断；unknown 不生成 root Session。 |
| T-S02-012 | 独立闭环 | P0 | S02 完成门 | 多来源字段优先级：state name>title>session index；rollout cwd>state cwd；低优先级 null/state 暂不可用/旧事实均不覆盖可信值。 |
| T-S02-013 | 独立闭环 | P0 | S02 完成门 | metadata provenance/record offset/conflict：parent 至少区分 `session_meta_parent` / `subagent_source` / `forked_from_id`，优先级固定；同 provenance 第一可信记录；direct/nested/forked 同值为一致证据，不同值按优先级选择并标 conflict；role provenance 冲突稳定；一轮同 Thread 至多一 patch，无变化无 patch。 |
| T-S02-014 | 前置联动：S01 | P0 | S02 完成门 | 真实 Ledger safe-fact 集成：generation/parser/offset/binding/owning ID 全匹配才 Matching；state/index patch-only 与 rollout 完整 fact 输入边界正确。 |
| T-S02-015 | 前置联动：S01 | P0 | S02 完成门 | 真实 metadata commit：TokenCount/Ignored 的 patch=None 仍推进 fact/checkpoint且不增 data revision；group/patch/owning ID/CAS 不一致整组回滚；confirmed binding+patch+checkpoint 原子。 |
| T-S02-016 | 前置联动：S01 | P0 | S02 完成门 | 绑定/continuation 错误路径：已绑定 ID 冲突不覆盖且不推进；owning 未确认不绑定/不推进；metadata 失败不推进 offset；usage offset 不被 metadata 推进。 |
| T-S02-017 | 前置联动：S01 | P1 | S02 完成门 | 重复 patch 不增 revision；事实变化每事务最多一次 revision；多个来源最终归一 Thread。 |
| T-S02-018 | 独立闭环 + 前置联动：S01 | P0 | S02 完成门 | 隐私矩阵：正文 sentinel 不进入 fact/patch/DB/log；state SQL 不引用 preview/first_user_message；JSON 错误不打印原行；不读取用户项目文件。 |
| T-S02-019 | 后置联动：S03 | P0 | S03 完成门 | 用真实 scanner 验证 S02 continuation/safe-fact：offset0、nonzero、重启、foreign meta、unchanged skip，以及 `payload.parent_thread_id`-only Guardian 的 direct parent/root；旧 metadata parser fact 不能跨 parser version 复用。 |
| T-S02-020 | 后置联动：S04 | P0 | S04 完成门 | ownership/parent/root 真正交给 usage consumer：父 replay Token 不计入、OwningLive 后子 Token 正确计入；`payload.parent_thread_id`-only Subagent 即使 state 无 spawn edge 也能解析 root；late foreign meta 触发 usage rebuild。 |
| T-S02-021 | 独立闭环 | P0 | S02 完成门 | 真实 Codex session_meta 结构回归：使用 2026-08-09 三份真实 rollout 脱敏后的结构等价 fixture，至少覆盖 main、legacy thread_spawn Subagent、Guardian `source.subagent.other + payload.parent_thread_id`；fixture 必须保留真实字段层级，不得用测试专用虚构路径替代 direct parent。 |
| T-S02-022 | 前置联动：S01/S03 | P0 | S03 完成门 | Metadata parser 版本契约：本次 parent 解析语义升级 v1→v2；旧 v1 `ready` checkpoint/safe fact 在 v2 下必须 mismatch，从 offset0 重放并产生 `session_meta_parent` provenance；不得只修改版本号或沿用旧 fact。 |

## 5. Spec03 测试条目

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-S03-001 | 前置联动：S01 | P1 | S03 完成门 | 协调器 cadence：startup 立即扫描、timer 首次不重复；默认300秒与60～3600配置；missed ticks 不补跑；扫描中增长不自行触发即时 follow-up。 |
| T-S03-002 | 前置联动：S01 | P0 | S03 完成门 | Manual/timer coalescing 与 Started 线性化：running 期间只一个持久化 follow-up；Started 必须晚于 started commit，revision 对应；commit 失败不返回 Started。 |
| T-S03-003 | 前置联动：S01 | P0 | S03 完成门 | follow-up 状态机：当前终态后 queued 仍可见；启动事务原子清 queue/设 active；Busy 保持 queued 重试；非重试错误写 start_failed。 |
| T-S03-004 | 前置联动：S01 | P0 | S03 完成门 | scan lifecycle：started/completed/failed ID 配对、过期 ID 拒绝、无业务变化仅 status revision；shutdown 写入固定 active/queued 终态。 |
| T-S03-005 | 前置联动：S01 | P1 | S03 完成门 | 启动恢复代表矩阵：active-only、active+queued、idle/failed+queued、follow-up 提交边界、Busy retry 重启；旧 active→SCAN_INTERRUPTED，queued 优先 Startup，恢复前拒绝 request。 |
| T-S03-006 | 独立闭环 | P1 | S03 完成门 | discovery：两个区域递归；根不存在为空；权限错误不误标 missing；symlink/目录/扩展名过滤；filename UUID candidate 校验。 |
| T-S03-007 | 前置联动：S01 | P0 | S03 完成门 | 真实文件身份：rename 保留 source/generation/checkpoint；copy 新来源；same-path replacement/truncate/same-size rewrite bump generation 并使 consumer 重建；generation 清 binding，纯 rename 继承。 |
| T-S03-008 | 前置联动：S01 | P1 | S03 完成门 | 同物理 alias 一次处理；missing/重新出现正确；枚举顺序变化结果确定。 |
| T-S03-009 | 前置联动：S01 | P0 | S03 完成门 | 崩溃点矩阵：observation 后 parse 前、parse 后 commit 前、commit 后退出，分别从数据库已提交 offset 恢复，不漏/不重推进。 |
| T-S03-010 | 前置联动：S01 | P0 | S03 完成门 | 计划器与 consumer 隔离：offset>size/rewrite 重建；metadata parser version 改变只重建 metadata consumer，usage parser version 改变只重建 usage consumer；任一 consumer 不得借另一 consumer 的 checkpoint/fact 冒充已升级。 |
| T-S03-011 | 前置联动：S01/S02 | P0 | S03 完成门 | processing/error 恢复：可信 generation/identity/guard + OwningLive 可从旧 nonzero offset 重试；任一关键条件缺失则从0重建。 |
| T-S03-012 | 独立闭环 | P0 | S03 完成门 | fixed view：只读 discovery observed size；枚举后追加也不扩大；open 前后替换、读取中截断均不得提交。 |
| T-S03-013 | 独立闭环 | P0 | S03 完成门 | guard：非零 offset 的4096-byte guard 匹配/失配；offset0 guard=NULL。 |
| T-S03-014 | 独立闭环 | P0 | S03 完成门 | 行边界：LF/CRLF/空行、完整 EOF、half-line 不输出不推进、下一轮补全只输出一次。 |
| T-S03-015 | 独立闭环 | P1 | S03 完成门 | oversized 行有界处理：>8MiB 完整行有界丢弃并推进 metadata；>8MiB half-line 不推进；实现不得使用无界 read-to-end。 |
| T-S03-016 | 前置联动：S01/S02 | P0 | S03 完成门 | 真实 S02 ownership 扫描：offset0 main/Subagent、legacy fork replay→OwningLive、`payload.parent_thread_id`-only Guardian、重启 nonzero、late foreign meta→rebuild、unstable continuation 不绑定不推进。 |
| T-S03-017 | 前置联动：S01/S02 | P0 | S03 完成门 | safe fact 复用/重建：普通+归档同 Thread 一轮至多一 patch；unchanged skip 仅复用 generation/parser/offset/binding 全匹配 fact；fact 缺失或 metadata parser version mismatch 必须从0重建，旧 v1 parent 缺失 fact 不得继续 Matching。 |
| T-S03-018 | 前置联动：S01/S02 | P0 | S03 完成门 | 增量 fact 合并：字段时间、provenance、record offset 与 owning ID 三方一致；低优先级/后到同 provenance 冲突不覆盖可信值；不一致不能 Skip/续读/提交。 |
| T-S03-019 | 前置联动：S02 | P1 | S03 完成门 | state/session-index 每轮读取与降级：state 标题删除后回退 session-index；rollout fact 不完整时 state/index 不越权覆盖；state unavailable 不错误判 Main；malformed/unknown 不阻后续。 |
| T-S03-020 | 前置联动：S01/S02 | P1 | S03 完成门 | metadata consumer 遇 TokenCount 只推进 metadata，不由 metadata 路径创建 usage checkpoint。 |
| T-S03-021 | 前置联动：S01/S02 | P0 | S03 完成门 | Thread group 事务：safe facts/binding/patch/checkpoints 原子；patch=None 路径不增 data revision；group/patch/owning/CAS 冲突和注入失败整组回滚。 |
| T-S03-022 | 前置联动：S01/S02 | P1 | S03 完成门 | 隔离性：单 Thread 组失败不阻其他组；失败组不得 Clear/关系降级/投影改变；重复扫描幂等，多组成功可查；partial scan 最终 failed 但成功组稳定数据保留。 |
| T-S03-023 | 前置联动：S01/S02 | P0 | S03 完成门 | 日志与数据库全链路正文 sentinel 不可见。 |
| T-S03-024 | 独立闭环 + 前置联动：S01 | P1 | S03 完成门 | 增量效率核心：unchanged scan 只枚举/stat、不打开已完成 rollout；单文件 append 读取量接近新增区间+guard。 |
| T-S03-025 | 独立闭环 | P2 | 最终完整测试 | 性能基线：首次导入记录总字节/文件数/耗时/峰值内存；峰值正文缓冲受限于两个 line buffer+固定开销；报告不输出正文。 |
| T-S03-026 | 前置联动：S01/S02 | P0 | S03 完成门 | 真实 stale-safe-fact 修复链：预置 parser v1、offset=EOF、parent_hint=NULL 的 ready fact；启动 parser v2 后 unchanged rollout 仍必须从0重读 direct parent，原子更新 fact/checkpoint/thread parent/root，且 usage checkpoint 不被 metadata 路径擅自推进。 |
| T-S03-027 | 独立闭环 + 前置联动：S02 | P0 | S03 完成门 | metadata parser 单一版本源：`METADATA_PARSER_VERSION=2`；`ScanConfig`/main/环境/API 不含 metadata parser version 可配置入口；测试模拟 v1 只能 seed durable old fact/checkpoint。 |

## 6. Spec04 测试条目

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-S04-001 | 前置联动：S01 | P0 | S04 完成门 | SQLite Token schema 迁移链：v1 metadata → v2 usage ledger → v3 `NormalizedTokenUsage` → current v4；Spec04/Spec07 的 v2→v3 Token 数值无损，旧 Token 列从 current runtime schema 物理退出；v3→v4 不改变 canonical Token 数据；失败全回滚；inactive epoch 不参与 usage 查询。 |
| T-S04-002 | 独立闭环 | P0 | S04 完成门 | usage parser/canonical algorithm 一对一；当前 normalized Token 语义为 parser/canonical v3；canonical/event-ID/fingerprint 规则变化必须伴随版本 bump；旧 build/carry 算法不匹配拒绝。 |
| T-S04-003 | 独立闭环 | P0 | S04 完成门 | `NormalizedTokenUsage` 约束：`input_tokens/cached_tokens/cache_write_tokens?/output_tokens/reasoning_tokens/total_tokens` 非负与子集关系、`total=input+output`、checked add/sub/overflow；`Some(0)` 与 `None` 严格区分。 |
| T-S04-004 | 独立闭环 | P0 | S04 完成门 | Codex Raw Token → Normalized Adapter：`cached_input_tokens→cached_tokens`、`cache_write_input_tokens→cache_write_tokens`、`reasoning_output_tokens→reasoning_tokens`；完整 token_count、info=null、last 缺失；cache-write 显式数值（含0）→Some、缺失→None；不读取模型能力、不猜0。 |
| T-S04-005 | 独立闭环 | P1 | S04 完成门 | 生命周期/模型 raw 兼容：task/turn started/complete 别名、aborted/failed、缺 Turn ID/timestamp、稳定 synthetic key 与 block_time_missing；unknown fields/records 不阻断。 |
| T-S04-006 | 前置联动：S03 | P1 | S04 完成门 | usage adapter 延续 scanner 行边界：malformed/half-line/oversized；4–8MiB 合法完整行可独占批，>8MiB 完整行 bounded oversized-only 推进且不保留原文。 |
| T-S04-007 | 独立闭环 | P0 | S04 完成门 | chain interruption：invalid last 不 recovered；invalid total 不更新 baseline 并跨 checkpoint/restart 保持 interruption；malformed/oversized/ownership/parser gap 后首个 total 只建新 baseline。 |
| T-S04-008 | 独立闭环 | P0 | S04 完成门 | 累计 reset/invalid-last 处理：当前可信 total 原子成为新 baseline；required 维下降统一判 TOTAL_CHAIN_RESET；有效 last 仍只走 normal，无效/缺失不生成事件，不额外丢下一次。 |
| T-S04-009 | 独立闭环 | P0 | S04 完成门 | normal 事件：首次有效 last；重复 total 不计；total 增长+last 仅计 last，不同时恢复 cumulative delta；两个真实同数值请求仍因锚点不同各计一次。 |
| T-S04-010 | 前置联动：S03 | P0 | S04 完成门 | canonical 去重跨扫描重试/崩溃重读/rename/archive copy 稳定；sessions+archived 相同记录只计一次。 |
| T-S04-011 | 独立闭环 + 前置联动：S01 | P0 | S04 完成门 | canonical/occurrence 一致性：同 event ID payload 不同 hard conflict；同 canonical 不同 provenance 正常去重；每 candidate 有 occurrence；位置键冲突或 canonical 字段不一致拒绝且 checkpoint 不推进。 |
| T-S04-012 | 前置联动：S01/S03 | P1 | S04 完成门 | 副本独立 occurrence 维持可迁移性：A事件+B duplicate 后来源 replacement/missing/carry，B occurrence 能把 canonical event 带入新 epoch；unique race 回读比较不静默 ignore。 |
| T-S04-013 | 独立闭环 | P0 | S04 完成门 | recovered：last 真缺失且连续 cumulative 非负差才生成；previous 缺失/reset/ownership gap/任一 required 负差禁止；recovered 后 current 成新 baseline，同段不可 normal+recovered。 |
| T-S04-014 | 独立闭环 | P0 | S04 完成门 | recovered cache-write：两端 `Some` 可求差；任一 `None`→null；current<previous 产生 CACHE_WRITE_CHAIN_DECREASE 并阻断补偿但继续新 baseline。 |
| T-S04-015 | 独立闭环 | P0 | S04 完成门 | Turn compensation 核心矩阵：delta=accounted 不补；delta>accounted 只补缺口；accounted>delta/缓存负差/缓存 accounted 超 delta 只记 anomaly 不扣减；start/end/reset/time 缺失禁止。 |
| T-S04-016 | 独立闭环 | P0 | S04 完成门 | Turn 模型与 duplicate accounted：single model 归模型，多模型→unknown，none/unresolved 禁补；archive duplicate candidate 仍进入本来源 accounted 但不产生二次补偿。 |
| T-S04-017 | 独立闭环 + 前置联动：S01 | P0 | S04 完成门 | open Turn/accounted/cache-write Option/block reasons 跨进程恢复；count=0 与 cache-write `Some(0)` 初值、任一 `None` sticky；gap/start-missing/reset 等 block 跨重启保持。 |
| T-S04-018 | 前置联动：S02 | P0 | S04 完成门 | ownership 消费：top-level owning Token 计入；Subagent 文件 replay 的父 token/lifecycle/model 全排除；legacy nested-parent 与 `payload.parent_thread_id`-only Guardian 都必须在 OwningLive 后把子 Token 归正确 child/root；多层 Subagent 归最上 root。 |
| T-S04-019 | 前置联动：S01/S02/S03 | P0 | S04 完成门 | nonzero late foreign meta 整组零提交并 rebuild；root 未确认不推进；metadata parser 升级或 parent/root 后到后，usage 重新规划并只计一次，不能让旧 root-null state 永久阻塞 build。 |
| T-S04-020 | 前置联动：S01/S02 | P0 | S04 完成门 | confirmed root 变化原子 reconcile existing events/source states；有 build 时同事务 replacement 且保留旧 manifest 和未受影响 build-only progress。 |
| T-S04-021 | 前置联动：S01/S02/S03 | P0 | S04 完成门 | 首次 None→confirmed binding/root patch 与 safe facts/metadata checkpoints、active usage reconcile、build disposition 在同一 commit_metadata 事务，失败共同回滚，无事后 reconcile。 |
| T-S04-022 | 前置联动：S01 | P0 | S04 完成门 | usage commit 原子性：event/occurrence/turn/anomaly/state/checkpoint 任一失败全回滚；duplicate-only 可写 occurrence+推进 checkpoint 但不增 data revision；active 新事件事务至多+1 revision。 |
| T-S04-023 | 前置联动：S01/S03 | P0 | S04 完成门 | nonzero resume 只匹配 working epoch/parser state；generation/parser/offset/binding/root/chain/open Turn 任一不匹配不得续读；verified error 仅全条件可信时原子恢复 ready。 |
| T-S04-024 | 前置联动：S02/S03 | P1 | S04 完成门 | 超长 Subagent replay 前缀使用固定 classifier state；OwningLive 前崩溃从0重读且不产生历史 usage。 |
| T-S04-025 | 前置联动：S01/S03 | P0 | S04 完成门 | LocalReplaySafe 仅在 epoch/parser/generation/device/inode/binding/root/canonical algorithm 全匹配时允许；只替换本 source rows 并按 occurrence 清 orphan；任一变化转完整 epoch rebuild。 |
| T-S04-026 | 前置联动：S03 | P1 | S04 完成门 | usage 复用同一 fixed view：读取期间增长不扩张本轮边界，也不自行触发 follow-up。 |
| T-S04-027 | 前置联动：S01/S03 | P0 | S04 完成门 | build 中 source observation：新 present→manifest add/required boundary/checkpoint reset 同事务；冻结身份失配→完整 replacement 同事务，崩溃无跨表窗口。 |
| T-S04-028 | 前置联动：S01 | P0 | S04 完成门 | replacement 保留旧 manifest 全集、可信 proof/progress；missing 未完成保持 blocked；仅受影响来源按 occurrence 清理，retained rebuilt/pending-ready state 不被二次初始化清零。 |
| T-S04-029 | 前置联动：S01/S03 | P0 | S04 完成门 | manifest pending/blocked 与 working state：首次无 state→rebuild_required/0；已有 matching state 的中间批/同身份恢复可 ready 非零续读或 CompleteOnly。 |
| T-S04-030 | 前置联动：S03 | P1 | S04 完成门 | BuildFrom(0) 可多批提交并从 committed offset 续跑；无 build 的 LocalReplay(0) 若首批超预算必须零写入并转完整 build。 |
| T-S04-031 | 前置联动：S01/S03 | P0 | S04 完成门 | reader/storage batch contract：中间批 fixed_view_exhausted=false/tail=unverified 只推进完整行；真正 EOF 才产生 none/half_line；矛盾 exhausted/tail/计数组合被 storage 拒绝不推进。 |
| T-S04-032 | 前置联动：S01/S03 | P0 | S04 完成门 | raw-tail proof：同 generation/raw size 保留 verified；大小变化转 unverified；half-line boundary 在 tail start；补全单调推进；generation replacement 从0重建且旧 offset 不参与。 |
| T-S04-033 | 前置联动：S01 | P0 | S04 完成门 | active usage source state 在 build 激活删除 manifest 后仍保留 durable tail proof；下一 build missing carry 只接受 generation/raw size/proof 完整匹配。 |
| T-S04-034 | 前置联动：S01/S03 | P1 | S04 完成门 | SourceOutcome 五个 build disposition 都可达；carry-in-progress 同身份恢复 present 必须走 carry_resumed_present/ResumeCarry；S01/S04 枚举序列化一致。 |
| T-S04-035 | 前置联动：S01 | P0 | S04 完成门 | 首次 epoch：active=0 无 build 时先建 working epoch1，禁止写 epoch0；build 完整前 active 查询为空 stable 数据，激活后一次可见。parser bump/truncate 时旧 active 保持可查。 |
| T-S04-036 | 前置联动：S01/S03 | P0 | S04 完成门 | manifest durable coverage/activation：重启成员不丢；active contributor、build-start present、新发现 present 都必须 rebuilt 或可信 carried；任何 blocked/pending member 禁止激活；上游 metadata 修复使 root 可确认后，原 blocked member 必须可重新进入 rebuild 并最终允许 activation。 |
| T-S04-037 | 前置联动：S01/S03 | P0 | S04 完成门 | missing carry 基本闭环：仅同 parser 且 active identity/binding/checkpoint/state/tail/required boundary 匹配才 eligible；BeginCarry 单事务初始化，ResumeCarry 持久化 cursor 分批复制，finalize 才恢复 state/checkpoint 并标 carried。 |
| T-S04-038 | 前置联动：S01/S03 | P1 | S04 完成门 | partial BuildFrom 后来源 missing：BeginCarry(partial_seed) 退役 working state、重置 checkpoint、保留 seed 并从 active 首 key 全量验证；seed mismatch hard fail；关键事务失败保持前态。 |
| T-S04-039 | 前置联动：S01/S03 | P0 | S04 完成门 | build parser/身份变化 replacement：target parser 不同、generation/identity/binding 变化均 replacement；旧成员全集保留，受影响从0，未受影响 proof/progress 保留。 |
| T-S04-040 | 前置联动：S01/S03 | P1 | S04 完成门 | manifest 状态转换：pending↔blocked→rebuilt/carried、同身份 append/reappear、carry-in-progress present、completion-only、build append/tail reverify、required boundary 阻止错误 carry。 |
| T-S04-041 | 前置联动：S03 | P1 | S04 完成门 | 大文件基本有界性：LocalReplay 超单批转 build；BuildFrom 以4MiB/4096行/2048 candidates 上限多批，完整行边界提交并可从 committed offset 重启。 |
| T-S04-042 | 前置联动：S01 | P0 | S04 完成门 | activation 原子性：失败保持旧 active；成功只增加一次 revision；旧 epoch 清理中断不影响 active 查询；rebuild 前后聚合一致且无重复。 |
| T-S04-043 | 独立闭环 + 前置联动：S01 | P0 | S04 完成门 | 聚合数学：UTC [start,end)、跨日事件；summary `total_tokens=input_tokens+output_tokens`；`cached_tokens`、nullable `cache_write_tokens`、`uncached_input_tokens`、`reasoning_tokens`、`other_output_tokens`、token-weighted `cache_hit_rate` 在 Summary/Session/model 一致归并。 |
| T-S04-044 | 前置联动：S02 | P0 | S04 完成门 | Session/root 聚合：仅 root 成行并含多层 Subagent；self/subagent/inclusive/subagent_count、models_used 稳定去重且 unknown 不猜测。 |
| T-S04-045 | 独立闭环 | P0 | S04 完成门 | 跨视图聚合不变量：model required sums=summary；Session inclusive sums=summary；session_count=root 行数；estimated_cost 恒 null；SQL SUM 溢出返回错误。 |
| T-S04-046 | 独立闭环 + 前置联动：S02/S03 | P0 | S04 完成门 | usage 隐私：正文 sentinel 不进 DB/log/diagnostics；不读用户项目目录；不存 rate-limit payload/完整 JSON。 |
| T-S04-047 | 前置联动：S03 | P1 | S04 完成门 | 增量资源核心：unchanged 且 checkpoint/state 匹配正文读取0；普通 append 读取量接近新增区间+guard；聚合查询不把全历史事件加载内存，Session 使用分页。 |
| T-S04-048 | 前置联动：S01/S03 | P2 | 最终完整测试 | 计划优先级穷举：parser bump+carry、关系未确认、offset>raw、offset=raw+unverified、offset<raw+ready 等冲突条件验证前置分支优先级。 |
| T-S04-049 | 前置联动：S01/S03 | P2 | 最终完整测试 | Carry 四阶段断点 × present 恢复：occurrences/turns/anomalies/finalize 各断点继续 ResumeCarry，完成后追平新增或 CompleteOnly，全程无 duplicate/漏 occurrence。 |
| T-S04-050 | 前置联动：S01/S03 | P2 | 最终完整测试 | Carry 四阶段断点 × generation/inode/binding/active-prefix guard 失配：均执行 replacement、清 partial carry/orphan、受影响 BuildFrom(0)，其他 manifest 成员/proof/progress 不变。 |
| T-S04-051 | 前置联动：S01/S03 | P2 | 最终完整测试 | 大型 LocalReplay/Carried 多批压力：LocalReplay 超预算零写转 build；Carried 各 phase 任意批间崩溃可从 cursor 恢复且最终批前保持 rebuild_required/pending。 |
| T-S04-052 | 前置联动：S02/S03 | P2 | 最终完整测试 | 1GiB 级 synthetic rollout 与超长 replay 前缀资源测试：批数量随输入增长但单批 bytes/lines/candidates、窗口内存和进程峰值在预算内，不缓存全部 batch。 |
| T-S04-053 | 前置联动：S01/S02/S03 | P0 | S04 完成门 | 真实 blocked-build 恢复链：构造 state 无 spawn edge、rollout 仅 `payload.parent_thread_id` 的 Subagent；旧 metadata fact 导致 build member root=NULL/blocked 后，metadata v2 重建修复 parent/root，usage build member重新 rebuilt/carried，manifest 全完成后 activation 原子成功，active epoch 从旧值切换到 build epoch。 |
| T-S04-054 | 独立闭环 | P0 | S04 完成门 | production carry 单实现 Gate：不存在 `src/usage/carry.rs` / `pub mod carry` reference state machine；fresh/partial seed/resume/present-during-carry/finalize/reopen/mismatch 等有效语义全部直接针对 `src/storage/usage.rs` 与真实 SQLite integration 测试。 |

## 7. Spec05 测试条目

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-S05-001 | 独立闭环 | P0 | S05 完成门 | 五个 named range 的 UTC 边界算法：普通日期、月/年边界、DST overlap/gap；overlap 取较早 instant、gap 取后一有效 instant。 |
| T-S05-002 | 独立闭环 | P2 | 最终完整测试 | 极端本地时间：整日跳过可得到零长 range，以及罕见 tzdb 边界组合。 |
| T-S05-003 | 独立闭环 | P1 | S05 完成门 | IANA 名称缺失、tzdb 不可用或转换失败统一返回 LOCAL_TIME_UNAVAILABLE；未知 range/非法 limit/cursor 返回固定安全错误。 |
| T-S05-004 | 前置联动：S04 | P0 | S05 完成门 | summary/Session JSON canonical 映射：`cached_tokens`、nullable `cache_write_tokens`、nullable `uncached_input_tokens`、`reasoning_tokens`、`other_output_tokens`、`total_tokens`、`cache_hit_rate` 与 estimated_cost；Some(0)/None 不混淆；不得返回旧 `cached_input_tokens/cache_write_input_tokens/cache_write_status/cache_tokens/reasoning_output_tokens`。 |
| T-S05-005 | 前置联动：S04 | P0 | S05 完成门 | Session keyset cursor：排序、limit+1、next、第二页、HMAC 篡改/错版/进程重启失效、跨 revision/range 失效。 |
| T-S05-006 | 前置联动：S04 | P1 | S05 完成门 | model 排序 total desc/model asc 且 unknown 保留。 |
| T-S05-007 | 前置联动：S01/S04 | P0 | S05 完成门 | 每个 usage response 的 data_revision、active_epoch 与查询结果来自同一只读快照。 |
| T-S05-008 | 前置联动：S01/S03 | P0 | S05 完成门 | status 映射：idle/running/failed、last-finished、followup null/queued/start_failed、首次导入/source_changed/nullable times 全部准确。 |
| T-S05-009 | 前置联动：S03 | P0 | S05 完成门 | refresh：缺/错 header→403；Started→202+直接 scan ID/started revision；running→200 Coalesced+follow-up ID/enqueue revision；只等待持久化 ack 不等待扫描 I/O/完成。 |
| T-S05-010 | 前置联动：S01/S03 | P0 | S05 完成门 | refresh/status 目标追踪：终态到 follow-up started 间 status 仍 queued；重载/断网可恢复 target ID；多个 Coalesced 共 ID；failed/idle 可重新 Started，running/source_changed/queued 不并发。 |
| T-S05-011 | 前置联动：S03 | P1 | S05 完成门 | Busy 保持 queued 并内部重试/重启恢复；仅 nonretry internal/shutdown/source_changed 映射 start_failed 安全码。 |
| T-S05-012 | 前置联动：S01/S03 | P1 | S05 完成门 | target_scan_id 历史可追：T 后连续 F/G 仍返回 T；start_failed 后新 scan 不覆盖旧 target；重启/慢通知不影响 target row，同一快照返回当前投影+target。 |
| T-S05-013 | 前置联动：S03 | P0 | S05 完成门 | started commit 失败不伪造202；recovering/shutdown/source_changed 固定安全错误；source_changed=409 且 scanner 不收到请求。 |
| T-S05-014 | 前置联动：S01/S03/S04 | P1 | S05 完成门 | SSE 首连立即发当前 revision tuple；data-only/status-only/同时变化各发布一次最新值；断线后 /api/revision 可发现相同变化。 |
| T-S05-015 | 前置联动：S01/S03/S04 | P0 | S05 完成门 | revision 只在数据库 commit 成功后 publish；失败不 publish；无订阅者不影响 commit。 |
| T-S05-016 | 前置联动：S01/S03/S04 | P2 | 最终完整测试 | SSE 背压/生命周期压力：慢 receiver 有界合并，断开无 task/queue 泄漏。 |
| T-S05-017 | 独立闭环 | P0 | S05 完成门 | 服务只监听 127.0.0.1:3210，不绑定 0.0.0.0。 |
| T-S05-018 | 独立闭环 | P0 | S05 完成门 | HTTP 边界安全：静态页/GET/SSE/refresh 均拒绝外部 Host、rebinding Origin、cross-site Sec-Fetch；合法 Host 无 Origin CLI 可用；无 wildcard CORS、API no-store、未知 /api/* 返回 JSON 404，页面路由才 fallback index。 |
| T-S05-019 | 前置联动：S01/S04 | P0 | S05 完成门 | SQLite busy、聚合溢出、内部错误仅返回固定安全 code；日志/响应不含 SQL、路径、Prompt、回复或原始 JSONL。 |
| T-S05-020 | 前置联动：S03/S04 | P0 | S05 完成门 | running/rebuild/failed 期间旧 stable usage 继续可读；active epoch 0 返回合法空结果。 |
| T-S05-021 | 前置联动：S01/S03/S04 | P1 | S05 完成门 | 代表性并发查询+扫描不产生部分结果或写锁死锁；同步 SQLite 查询从 Tokio executor 隔离。 |
| T-S05-022 | 前置联动：S01/S03/S04 | P2 | 最终完整测试 | 并发压力下持续查询/扫描/refresh/SSE，验证无锁死、无 executor 饥饿、无资源增长。 |
| T-S05-023 | 前置联动：S01/S03 | P0 | S05 完成门 | current Status API 契约：不得返回或依赖 `last_full_import_completed_at_ms`；首次导入/同步可用性只由真实 scan/follow-up/source-binding/usage epoch 状态投影，Frontend DTO 也不得保留该死字段。 |

## 8. Spec06 测试条目

| ID | 依赖分类 | 优先级 | 执行点 | 测试条目 |
|---|---|---:|---|---|
| T-S06-001 | 独立闭环 + 前置联动：S05 | P1 | S06-01 完成门 | 前端工程与构建边界：保留 React19 + TS strict + Vite6 + Tailwind4；Vitest/jsdom/RTL 为最低必需组件测试依赖，若真实浏览器验收需要额外 runner 只能作为 test/dev dependency；无 router/全局状态库/请求缓存/UI/图表库；`test/check/build` 可执行；JetBrains Mono WOFF2 + 许可本地入库、无 CDN；Axum 可托管 `frontend/dist`。 |
| T-S06-002 | 前置联动：S05 | P0 | S06-01 完成门 | `usagiClient` 对 summary/status/revision/refresh/error 使用精确 canonical DTO 与运行时校验：HTTP status、必需字段、安全整数/ratio/null；`cache_write_tokens/uncached_input_tokens` nullable；不得依赖或 fallback 到旧 Token API 字段，不向 view 暴露原始 body/API message，不使用 `any`。 |
| T-S06-003 | 前置联动：S05 | P0 | S06-01 完成门 | 初始加载与 range snapshot：mount 默认 today 并行 summary+status；五 range 各自最多一份成功 snapshot；切换立即更新选择、abort 旧 summary、generation 丢弃晚响应；新 range 无缓存只显示 skeleton，失败不得借用其他 range；切回旧 range 可保留自己的旧值；`retry_load()` 只重试当前失败依赖并追平 revision。 |
| T-S06-004 | 前置联动：S04/S05 | P0 | S06-01 完成门 | 8 张 KPI 固定顺序/标签/字段：预估费用、总 Token、输入 Token、输出 Token、会话数量、缓存命中率、缓存写入 Token、缓存读取 Token；active epoch=0 合法零值；estimated_cost/null ratio/null cache-write 为 `—`；明确 cache-write 0 显示0；compact 值 title/accessible name 保留完整整数。 |
| T-S06-005 | 独立闭环 | P1 | S06-01 完成门 | 真实浏览器布局矩阵：1512px 时 84px 外留白、1344px 流体内容区、固定 `32 16 64` padding、1312px KPI 可用宽、237×106 卡片、5 列间 31.75px；1280/1024/768 自动换列；767/390 两列流体、无 body 水平滚动；页面无 nav/sidebar/settings/chart/图表占位。 |
| T-S06-006 | 独立闭环 | P1 | S06-01 完成门 | Dashboard 可访问性/动效：range `role=group` + aria-label/aria-pressed；按钮键盘可操作与 2px focus-visible；loading/error/status `aria-live=polite`、skeleton aria-hidden；reduced-motion 关闭 transition；200% zoom 与高对比仍可操作。 |
| T-S06-007 | 前置联动：S03/S05 | P0 | S06-01 完成门 | refresh 可点击条件与请求边界：status 已成功、binding ready、无 requesting/target/active/queued 时才允许；idle/failed 可新 Started，running/source_changed/queued 禁用；POST 必带 `X-Usagi-Request:1`；快速重复点击只有一个在途 refresh。 |
| T-S06-008 | 前置联动：S03/S05 | P0 | S06-01 完成门 | Started/Coalesced target 语义：202 保存直接 scan ID；200 保存 durable follow-up ID 而非当前 active ID；两者进入同步中；仅匹配 `target_scan_id` 的 queued/running/completed/failed/start_failed 归约目标，当前 scan 投影或单纯 revision 增长不能提前完成。 |
| T-S06-009 | 前置联动：S03/S05 | P0 | S06-01 完成门 | refresh/status 错误与竞态：403=`无法发起同步`，409 SOURCE_CHANGED=`数据源已变化`，其他 POST=`同步失败`；同步跟踪 status 失败=`同步状态获取失败` 且 `retry_refresh_status()` 只重试 status；POST 网络中断/旧 POST 响应晚到不得覆盖新 generation；普通 status 失败不得误报同步失败。 |
| T-S06-010 | 前置联动：S03/S05 | P0 | S06-01 完成门 | mount/重载目标恢复与 follow-up 边界：queued/start_failed/active 按持久化 ID 恢复；start_failed 先带 ID 查询 target 再归约；多个 Coalesced 共目标 ID；终态到 follow-up started 间仍 queued；Busy 保持等待并接受后续重试；只有 nonretry internal/shutdown/source_changed 进入 start_failed。 |
| T-S06-011 | 前置联动：S01/S03/S05 | P0 | S06-01 完成门 | revision transport：mount 连接 `/api/events`；首/新 tuple 按 data/status 分量触发所需 summary/status 重取，相同 tuple 去重；SSE error 后只有一个 60s `/api/revision` fallback；`retry_load()` 可立即 retry revision 但成功不停止 fallback；EventSource 恢复并收到有效 tuple 后停 timer；unmount/StrictMode 不遗留 EventSource/timer/fetch。 |
| T-S06-012 | 前置联动：S05 | P0 | S06-01 完成门 | Vite dev proxy 真实联动：`/api` 转发到 `127.0.0.1:3210`、`changeOrigin:true`，proxyReq 把 GET/SSE/refresh Origin 固定为 `http://127.0.0.1:3210`；通过真实 Vite dev server 请求 summary/events/refresh 均满足 S05 Host/Origin/Sec-Fetch 防护；production 只用同源相对 URL。 |
| T-S06-013 | 前置联动：S04/S05 | P0 | S06-01 完成门 | stable snapshot 语义：8 KPI 成功响应一次性替换；running/rebuild/failed 保留旧 stable KPI，不闪回全 0；active epoch=0 显示合法零值；当前 range 有旧 snapshot 的 refresh/error 保留该 snapshot，布局尺寸不变。 |
| T-S06-014 | 独立闭环 + 前置联动：S05 | P0 | S06-01 完成门 | 前端隐私/安全边界：不向远端/CDN/analytics 发请求；字体本地；API 使用同源相对 URL；错误 UI/console 测试不得泄漏原始 response、SQL、Prompt/回复/JSONL；不把 usage/status/refresh target 写 localStorage/IndexedDB；无设置/远程连接入口。 |
| T-S06-015 | 前置联动：S03/S05 | P2 | 最终完整测试 | Dashboard 生命周期/并发资源压力：快速 range 切换、重复 refresh、SSE 断连恢复、StrictMode mount/unmount 与 revision burst 组合；EventSource/timer/在途 fetch 有界，无 stale range/target 写入、无请求风暴。 |
| T-S06-016 | 前置联动：S04/S05/S06-01 | P0 | S06-02 完成门 | Session DTO/runtime validation：三个 usage scope 使用同一 canonical Token schema；range.key 必须匹配请求；revision/time/Token/subagent_count 为安全整数；nullable cache-write/uncached、ratio 0..1、cursor null/非空、estimated_cost 仅 null；同 response root_session_id 唯一；cursor opaque。 |
| T-S06-017 | 前置联动：S04/S06-01 | P1 | S06-02 完成门 | Session formatter：title/project null/blank 固定 fallback；models `[]/1/N` 为 unknown/首项/首项+N 且完整列表 accessible；输入/输出/推理复用 06-01 compact formatter；cache ratio/cost 复用同一逻辑；last_activity 使用 response IANA timezone 的同日/同年/跨年格式与完整 title，timezone 构造失败为数据错误。 |
| T-S06-018 | 前置联动：S05/S06-01 | P0 | S06-02 完成门 | 抽取共享 `revisionFeed` 后页面进程只有一个 EventSource/一个 fallback timer；第一个订阅建立 transport、最后一个释放清理；tuple 两分量分别单调且至少一项增长才发布；`retry_now()` 不建第二 timer；Dashboard 与 Session 两订阅者共享同一 feed，06-01 `DashboardViewModel`、summary/status/refresh target 语义全部保持不变。 |
| T-S06-019 | 前置联动：S05/S06-01 | P0 | S06-02 完成门 | Session 首屏/range snapshot：mount 请求 `today&limit=50`；五 range 各保留自身内存 snapshot；新 range 无缓存显示 6 行 skeleton，切回旧 range 先显示自己的 rows 并刷新；first-page/load-more 各自 generation+AbortController；range 切换取消两类旧请求；A 慢响应不得写入 B；Session 与 KPI 请求独立失败、互不清空成功数据。 |
| T-S06-020 | 前置联动：S05/S06-01 | P0 | S06-02 完成门 | Session revision 追平：feed.data_revision 大于当前 snapshot 才重取第一页；相同不请求、较小不倒退；Session 自己先取得更高 revision 时接受；revision 到达时取消 load-more；已有 first-page refresh 时同一 revision 不重复请求；refresh 完成后的共享 feed 同时驱动 KPI 与 Session 追平。 |
| T-S06-021 | 前置联动：S05 | P0 | S06-02 完成门 | 正常分页：仅 ready+idle+next_cursor 且无 first-page refresh 时 load-more；固定 limit=50；冻结 base range/revision/cursor；仅 range、snapshot revision、cursor、response revision、generation 全匹配才 append；新页与已有 rows/root ID 均不得重复；double click 只有一个请求；next_cursor=null 后 footer 消失。 |
| T-S06-022 | 前置联动：S05 | P0 | S06-02 完成门 | `409 STALE_CURSOR` / `400 INVALID_CURSOR`：废弃当前分页 generation，保留旧 rows，自动从当前 range 第一页恢复，成功原子替换并得到新 cursor；自动恢复最多一次；恢复首屏失败转普通 update error；不得修复/解析 cursor 或无限循环。 |
| T-S06-023 | 前置联动：S05/S06-01 | P1 | S06-02 完成门 | 普通 load-more 失败保留 rows 和 cursor、`page_state=error`，`retry_load_more()` 使用同一 snapshot cursor；重试前如 feed 已更高 revision，则优先整表第一页刷新而不再使用旧 cursor；首屏/refresh/page 错误文案和 retry seam 互不混淆。 |
| T-S06-024 | 前置联动：S02/S04/S05 | P0 | S06-02 完成门 | 表格数据口径与列契约：固定9列“最后活动/标题/项目/模型/输入 Token/输出 Token/推理 Token/缓存命中率/预估费用”；只展示 root Session，可见用量只读 `inclusive_usage` 的 canonical 字段；不展示/重算 self/subagent/subagent_count/root ID/project_path/cache-write/cached/total；接受服务端排序。 |
| T-S06-025 | 独立闭环 | P1 | S06-02 完成门 | loading/empty/refresh/error/footer 状态：首屏表头立即可见+6×48px skeleton；空成功跨 9 列且最小高度192；有 rows refresh 保留旧行+aria-busy、不上 skeleton；无缓存失败/有缓存更新失败/分页失败分别有稳定文案与正确 retry；loading-more disabled；无 next_cursor 不占 footer。 |
| T-S06-026 | 独立闭环 + 前置联动：S06-01 | P1 | S06-02 完成门 | 真实浏览器 Session 布局：KPI 后 32px 为标题、12px 后 table；surface token 正确；table `width:100%; min-width:1232px; table-layout:fixed` 与 9 列宽；1512 完整可见；窄屏仅 wrapper 横滚、body/标题/footer 不横滚，footer 始终可操作；长 title/project/model 不撑宽。 |
| T-S06-027 | 独立闭环 | P1 | S06-02 完成门 | Session 可访问性：原生 table/thead/tbody/th scope；section aria-labelledby；wrapper 可键盘聚焦且名称为“Session 记录表格，可横向滚动”；状态 aria-live、busy；ellipsis/compact/model +N 可取得完整值；行不做 click target；200% zoom body 不横溢；reduced-motion 无新 transition。 |
| T-S06-028 | 前置联动：S03/S04/S05 | P0 | S06-02 完成门 | 真实 API 集成：第一页 50+1 得 cursor，第二页保持服务端排序；revision 中途变化旧 cursor→STALE_CURSOR，server 重启旧 cursor→INVALID_CURSOR；空 range 正确；cache hit null/cost null 正确；running/rebuild/failed 继续返回旧 stable rows；API 安全错误只映射固定用户文案。 |
| T-S06-029 | 前置联动：S03/S05/S06-01 | P0 | S06-02 完成门 | Dashboard + Session 页面集成：KPI 后直接 Session记录，中间无图表/占位；同一个 range 同时驱动两套请求但失败隔离；同步 target 完成→共享 revisionFeed→summary 与 Session 第一页均追平；页面内仍最多一个 EventSource/一个 fallback timer；无新 API、DB 表或浏览器持久化 cache。 |
| T-S06-030 | 前置联动：S03/S05/S06-01 | P2 | 最终完整测试 | Session/前端综合压力：50→100→150→200 行分页、快速 range 切换、load-more 与 revision 同时发生、SSE 断连/恢复、同步完成并发；无跨 range/revision 混行、无旧 cursor 继续使用、无重复长期 transport/请求风暴，滚动/交互和内存维持预先定义预算。 |

## 9. 数据口径改造统一专项条目

以下 T-DC 条目已并入本文，不再把独立 `Usagi_测试标准_数据口径改造.md` 当作第二套标准。

| ID | 归属 Spec | 优先级 | 测试条目 |
|---|---|---:|---|
| T-DC-001 | S04 | P0 | NormalizedTokenUsage 正常构造：标准六字段成功，值完全一致。 |
| T-DC-002 | S04 | P0 | Normalized 非负矩阵：required 字段和 known cache-write 负值拒绝，cache_write=None 合法。 |
| T-DC-003 | S04 | P0 | 输入子集：cached<=input；known 时 cached+cache_write<=input。 |
| T-DC-004 | S04 | P0 | 输出子集：reasoning<=output。 |
| T-DC-005 | S04 | P0 | 唯一 total：total=input+output；不存在 reported/derived 双 total。 |
| T-DC-006 | S04 | P0 | cache-write 两态：Some(0) 与 None 严格不同。 |
| T-DC-007 | S04 | P0 | derived：known 时 uncached=input-cached-write；unknown 时 uncached=None；other_output=output-reasoning；hit=cached/input，input0→None。 |
| T-DC-008 | S04 | P0 | checked_add：required 正确相加；Some+Some→Some；任一 None→None；overflow 错误。 |
| T-DC-009 | S04 | P0 | checked_sub：required 非负差；Some-Some cache-write 正差；任一 None→None；负差错误。 |
| T-DC-010 | S04 | P0 | fingerprint/canonical v3：值或 Some/None 改变 fingerprint；版本稳定。 |
| T-DC-011 | S04 | P0 | Codex raw→canonical 全字段映射，raw 名称只在 Adapter 边界存在。 |
| T-DC-012 | S04 | P0 | raw cache-write=0 → Some(0)。 |
| T-DC-013 | S04 | P0 | raw cache-write 缺失 → None，不猜0，不查模型能力。 |
| T-DC-014 | S04 | P0 | raw required 字段缺失/字符串/浮点/负数/超 i64 均 Invalid。 |
| T-DC-015 | S04 | P0 | raw canonical invariant：cached>input、reasoning>output、total不等、known write越界均 Invalid。 |
| T-DC-016 | S04 | P0 | total_token_usage/last_token_usage 均经同一 Adapter；last缺失保持 Missing。 |
| T-DC-017 | S04 | P1 | usage parser 边界无 exact-model capability matrix；TurnContext model 仅作事件模型归属。 |
| T-DC-018 | S04 | P0 | normal event 使用 NormalizedTokenUsage，Adapter 后字段全部 canonical。 |
| T-DC-019 | S04 | P0 | recovered delta + known cache-write 六字段正确。 |
| T-DC-020 | S04 | P0 | recovered delta + unknown cache-write：required delta 保留，write=None，无 capability conflict。 |
| T-DC-021 | S04 | P0 | known cache-write chain decrease 仍检测 CACHE_WRITE_CHAIN_DECREASE。 |
| T-DC-022 | S04 | P0 | 任一 cache-write=None 不伪造 decrease/capability conflict。 |
| T-DC-023 | S04 | P0 | Turn accounted 中任一 write=None → aggregate write=None，required tokens 仍累计。 |
| T-DC-024 | S04 | P0 | Turn compensation start/last/accounted 使用同一 canonical 类型；unknown 不猜0。 |
| T-DC-025 | S04 | P1 | usage parser/canonical v3 rebuild gate：旧 v2 state 不作为 v3 continuation。 |
| T-DC-026 | S01/S04 | P0 | 隔离验证 Spec07 的 0→v3 canonical Token migration：v3 正式 Token 表无旧 Token 列；current fresh-open latest 仍必须继续执行 0004 到 v4，由 T-S01-027/T-FINAL-002 验证。 |
| T-DC-027 | S01/S04 | P0 | 隔离验证 v2→v3 canonical 数值/occurrence/turn/source-state 无损与废弃 capability anomaly 清理；不得把 v3 当作 current latest。 |
| T-DC-028 | S01/S04 | P0 | 隔离验证 0003 中途失败时完整回滚到 v2 schema/data/user_version；0004 的 rollback 另由 T-S01-027 覆盖。 |
| T-DC-029 | S01/S04 | P0 | migration 不把旧 usage parser/canonical 版本伪装成 v3。 |
| T-DC-030 | S04 | P0 | canonical storage CRUD/CAS/carry/reopen；known/unknown cache-write 往返一致。 |
| T-DC-031 | S04/S05 | P0 | Summary/Model 聚合 canonical；cache_hit_rate 为 token-weighted；任一 write unknown→aggregate write/uncached None。 |
| T-DC-032 | S04/S05 | P0 | Session self/subagent/inclusive 三 scope 使用同一 schema；inclusive 基础字段相加，derived 各自重算。 |
| T-DC-033 | S05 | P0 | API canonical 契约：返回新字段，nullable 原样；不得返回 legacy Token 字段；JSON safe integer/ratio 校验保持。 |
| T-DC-034 | S06 | P0 | Frontend canonical 契约：DTO/client/KPI 仅使用新字段；null≠0；中文标签正确；无旧 response fallback。 |
| T-DC-035 | S01～06 | P0 | 静态旧定义 Gate：运行时代码/API/Frontend 不存在 cache_tokens、CacheWriteStatus、cache_write_status、reported/derived total 等；Codex raw 字段只允许 raw Adapter/fixture/历史 migration。 |
| T-DC-036 | S01～06 | P0 | 旧方案冗余清理 Gate：`src/usage/carry.rs`、domain 兼容 aliases、processor `UsageCheckpoint/UsageStateProof/LoadedUsagePlanState`、`ScanEvent` wrapper、parser1 current-canonical mapping、current `app_meta.metadata_parser_version`、`last_full_import_completed_at_ms` 均不得存在于 runtime/current schema/API/Frontend；历史 migration/旧库 fixture 例外必须显式限定。 |

## 10. 最终完整测试

| ID | 优先级 | 测试条目 |
|---|---:|---|
| T-FINAL-001 | P0 | 全量回归：重新运行 Spec01～06 所有 P0/P1 自动化测试（含前端 `npm run test/check/build` 与要求的真实浏览器 Gate）；任何失败都阻塞发布。 |
| T-FINAL-002 | P0 | Fresh install E2E：空 DB→迁移到 current latest→应用启动→startup scan→metadata+usage 完成→active epoch 激活→status/summary/sessions/models API 一致可读；不得依赖历史旧字段或旧 parser fact。 |
| T-FINAL-003 | P0 | Incremental append E2E：已有稳定数据上追加 rollout，单次扫描只处理新增完整区间，metadata/usage 均正确推进且 API revision/结果一次更新。 |
| T-FINAL-004 | P0 | Identity/dedup E2E：rename、sessions↔archived duplicate、copy、same-path replacement/truncate，验证 source identity、generation、canonical usage 去重和必要 rebuild。 |
| T-FINAL-005 | P0 | Subagent/root E2E：main + legacy nested-parent Subagent + `payload.parent_thread_id`-only Guardian + 多层 Subagent + replay + late parent；验证 metadata direct parent/root 与 usage ownership 一致，Session inclusive/self/subagent 聚合正确。 |
| T-FINAL-006 | P0 | Crash/restart E2E：scanner commit、metadata parser-version rebuild、usage build/carry/activation 各关键边界强制退出并重启；最终数据不漏不重且旧 stable 数据在新 epoch 激活前始终可查。 |
| T-FINAL-007 | P0 | Manual refresh + status + SSE E2E：Started/Coalesced/follow-up/target tracking/revision publish/断线轮询恢复从真实 HTTP 入口闭环。 |
| T-FINAL-008 | P0 | CODEX_HOME change E2E：运行中/重启后 fingerprint 改变，采集与 refresh 被安全拒绝，旧 stable 数据不混写。 |
| T-FINAL-009 | P0 | 隐私/安全 E2E：fixtures 注入正文/Prompt/回复/未授权路径 sentinel；DB、日志、diagnostic/API/UI 不出现契约外泄漏；Spec05/06 明确允许的本机 `project_path` 只按正式 Session DTO/本地 tooltip 契约出现，不扩散到日志、analytics 或远程请求；Host/Origin/Sec-Fetch/CORS/loopback 规则从真实 server 验证。 |
| T-FINAL-010 | P1 | API snapshot/cursor 稳定性 E2E：扫描或 rebuild 并发发生时翻页/查询始终绑定同一 revision+active epoch；cursor 跨 revision 失效可预期。 |
| T-FINAL-011 | P2 | 运行全部 Spec P2：性能基线、极端 timezone、计划优先级、carry 组合、SSE/并发压力，以及 T-S06-015/030 的前端生命周期、分页与浏览器资源压力。 |
| T-FINAL-012 | P2 | 1GiB/大文件资源验收：首次导入与 rebuild 峰值内存、批大小、扫描跨度、查询内存与总运行时间记录；只验证预先定义的预算，不写“elapsed>=0”类无意义断言。 |
| T-FINAL-013 | P0 | Browser Dashboard E2E：真实 Axum + production/dev 前端从页面 mount 开始，验证 today KPI、range 切换、manual refresh Started/Coalesced/target 完成、SSE 更新、断线恢复轮询、页面 reload target 恢复，最终 UI 与 API revision/summary 一致。 |
| T-FINAL-014 | P0 | Browser Session E2E：真实 50+ Session 数据分页到多页；scanner/data revision 并发变化触发第一页追平；旧 cursor STALE、server restart INVALID 均自动恢复；任何时刻不混 range/revision、不重复 root Session、不清空旧 stable rows。 |
| T-FINAL-015 | P1 | Production UI/响应式/可访问性 E2E：Axum 托管实际 `frontend/dist`，在 1512/1280/1024/768/767/390 与 200% zoom 检查 Dashboard/Session 布局、wrapper-only 横滚、键盘/focus/aria/reduced-motion；确认字体/资源均本地且无外部 CDN/analytics 请求。 |
| T-FINAL-016 | P2 | Frontend concurrency/resource stress：rapid range + refresh + SSE flap + 200-row load-more + revision burst + StrictMode/reload 组合，按预设预算验证 EventSource/timer/fetch/DOM/内存有界且无 stale write、请求风暴或长期资源泄漏。 |
| T-FINAL-017 | P0 | 2026-08-09 真实故障回归 E2E：以三份真实 rollout 的脱敏结构 + 对应 state spawn-edge 事实（Guardian 无 edge）建立 fixture，验证 `payload.parent_thread_id → parent/root → usage rebuild → epoch activation → API → Dashboard` 全链；不得出现“build epoch 有新数据但 UI 长期读取旧 active epoch”的回归。 |
| T-FINAL-018 | P0 | Metadata parser upgrade E2E：从包含 v1 ready safe facts 的旧库启动新版本，必须自动重新解析受影响 rollout并升级到 metadata v2；未受影响数据保持稳定，usage 是否 reconcile/rebuild 按 root/binding 变化正确处理；不允许人工删库作为升级步骤。 |
| T-FINAL-019 | P0 | 测试标准/代码映射一致性 Gate：本文每个要求自动化的 P0/P1 条目都必须映射到当前存在的测试函数或明确新增测试；禁止引用已删除的 `TokenVector/CacheWriteStatus/cache_tokens` 等旧符号或已不存在的测试入口；独立执行记录不得把历史 PASS 冒充本版本 PASS。 |
| T-FINAL-020 | P0 | Current-contract 收口 Gate：latest schema=v4、metadata parser=v2 单一版本源、usage parser/canonical=v3、production carry 单实现、Status API 无 dead full-import 字段；active docs 只允许 v0.17 作为测试标准，Spec07 标记历史完成，Spec08 是当前唯一实施入口。 |

## 11. 本次真实故障的不可回退验收链

以下链路必须有**一个自动化跨模块测试**完整覆盖，不能拆成若干局部 PASS 后宣称完成：

```text
真实结构 Guardian rollout
  payload.parent_thread_id = ROOT
  source.subagent.other = guardian
  no nested thread_spawn parent
  no forked_from_id
  no state spawn edge
        ↓
metadata parser v2 从 offset 0 解析
        ↓
parent provenance = session_meta_parent
        ↓
threads.parent_thread_id / root_session_id 正确
        ↓
usage build member 不再 root=NULL blocked
        ↓
manifest 全 rebuilt/carried
        ↓
activation 原子切换 active epoch
        ↓
Summary/Sessions/Models 查询新 active epoch
        ↓
Dashboard 展示与 API 新 snapshot 一致
```

还必须建立旧库升级场景：metadata v1 fact 已 `ready` 且 offset=EOF、parent hint 为 NULL。新版本不得复用该 fact，必须因 parser mismatch 重放。

## 12. 静态旧定义与测试映射 Gate

最终验收至少执行：

```text
1. runtime canonical 层不得出现 cache_tokens / CacheWriteStatus / cache_write_status / reported_total_tokens / derived_total_tokens。
2. cached_input_tokens / cache_write_input_tokens / reasoning_output_tokens 只允许出现在 Codex raw Adapter、raw fixture、历史 migration 输入。
3. Spec02 parent provenance 不得把 session_meta_parent 伪装成 subagent_source。
4. `METADATA_PARSER_VERSION=2` 是当前唯一 metadata parser 版本源；main.rs / ScanConfig / 环境/API 不得暴露 parser version 配置入口。
5. v0.17 中每个 P0/P1 条目必须能映射到当前测试函数或本次新增测试；禁止引用已删除函数名。
6. current runtime/current schema/API/Frontend 不得存在 `src/usage/carry.rs` reference、dead compatibility aliases、`app_meta.metadata_parser_version` 或 `last_full_import_completed_at_ms`。
7. 执行记录必须来自本版本实际运行，不能复制 v0.15/v0.16 PASS。
```

### 12.1 S9 current implementation mapping

| 条目 | 当前自动化证据（本版本） |
|---|---|
| T-S06-016～017 | `frontend/src/data/usagiClient.test.ts`, `frontend/src/dashboard/session/sessionFormat.test.ts` cover Session DTO/runtime and timezone/title/model formatting. |
| T-S06-018～020 | `frontend/src/data/revisionFeed.test.ts`, `frontend/src/dashboard/session/useSessionTableController.test.tsx` cover one shared EventSource/fallback timer, monotonic revisions, range snapshots, first-page refresh and Session revision-error retry; `frontend/src/dashboard/DashboardPage.test.tsx` covers StrictMode remount lifecycle. |
| T-S06-021～025 | `useSessionTableController.test.tsx`, `SessionSection.test.tsx`, and `usagiClient.test.ts` cover fixed-50 cursor append, stale recovery, fixed errors/retry, nine-column inclusive contract, six skeleton rows, empty/loading/error/footer states. |
| T-S06-026～027 | `frontend/tests/browser/dashboard.spec.ts` `T-FINAL-014` checks the real Session table wrapper, nine headers, pagination and rendered rows in Chromium; `spec06_real_axum_browser_gate` runs the same test set against Vite dev and Axum production dist (9/9 each in the recorded run). |
| T-S06-028 | `tests/spec05_api_integration.rs::t_s06_028_real_http_session_pagination_revision_and_restart_contract` uses 51 scanner-produced roots, 50+1 pages, stale cursor after scan and INVALID cursor after router restart. |
| T-S06-029～030 | `DashboardPage.tsx` passes one `RevisionFeed` to Dashboard and Session; feed/controller tests cover shared transport, StrictMode cleanup, abort/generation and duplicate guards; browser `T-S06-030` runs real 50→100→150→200 pagination with scanner revision during load-more, blocked SSE transport and rapid range interaction against the Axum fixture. Dev and dist rounds each pass 9/9. |
| T-FINAL-014 | `frontend/tests/browser/dashboard.spec.ts` `T-FINAL-014 renders 50+1 Session pagination through the real Dashboard` runs against the same fixture and exercises scanner revision→STALE_CURSOR and server restart→INVALID_CURSOR; UI preserves stable rows and automatically recovers the first page before continuing. Dev and dist rounds each pass 9/9; backend stale/restart contract is also covered by `t_s06_028`. |
| T-FINAL-017 | `tests/spec06_frontend_browser.rs::spec06_real_axum_browser_gate` seeds one v3→v4 fixture containing Main (`payload.source` string), Legacy (direct `payload.parent_thread_id` + `payload.forked_from_id`), Guardian (direct parent + `source.subagent.other`, no state edge), and 200 extra Session rollouts (201 root Sessions). It begins a blocked build, waits for scanner replay/activation, asserts priority/conflict/root, active epoch/parser and Guardian `session_meta_parent` provenance, then the browser test queries the same real HTTP API and Dashboard. Dev and dist rounds each pass 9/9. |
| T-FINAL-018 | `src/scanner/mod.rs::tests::stale_guardian_fact_replays_from_zero_and_leaves_usage_checkpoint_untouched` proves v1 ready/EOF stale metadata replay, raw-byte and identity preservation, parser v2/fact/root repair, and usage checkpoint isolation; `tests/spec03_scanner_integration.rs::t_s03_017_missing_and_stale_safe_facts_force_real_worker_rebuild_from_zero` covers missing/stale safe-fact recovery. The browser fixture performs the same v1→v2 stale upgrade without manual deletion. |
| T-FINAL-019 | This table and `docs/test docs/Usagi_Spec06-02_测试代码布局_v0.1.md` map the newly implemented Session symbols to current tests. It does not mark unrelated final P0/P1 or P2 rows complete, and archived Spec04/Spec05 execution records are not current evidence. |

## 13. 必跑命令

后端：

```bash
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings   # 环境具备 clippy 时
```

前端：

```bash
cd frontend
npm test
npm run check
npm run build
npm run test:browser:gate
```

最终 E2E 还必须使用脱敏真实 rollout + 临时复制的 state/MU SQLite fixture，不得对用户真实数据库做写测试。

## 14. 完成判定

- Spec01～06 全部 P0/P1：PASS；
- T-DC-001～036：PASS；
- T-FINAL 全部 P0/P1：PASS；
- 全部 P2：最终发布门 PASS；
- 真实 Guardian 回归链：PASS；
- metadata v1→v2 stale safe-fact 升级链：PASS；
- 旧定义静态 Gate：PASS；
- 测试标准/代码映射一致性 Gate：PASS。

任何一项未完成，不得使用“旧测试曾 PASS”“旧 active epoch 仍有数据”“UI 能显示”作为放行理由。
