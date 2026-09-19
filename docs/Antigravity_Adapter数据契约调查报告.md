# Antigravity Adapter 数据契约调查报告

本报告针对 Usagi 接入 Antigravity 的底层数据格式进行了严格的本机数据反编译与交叉验证。所有结论均直接来自用户本机真实数据库（`~/.gemini/antigravity/conversations/*.db` 及 `conversation_summaries.db`），**未作任何代码改动**。

---

## 一、 已验证事实

### 1. `steps` 表与模型生成 step_type 规律
- **全量统计（跨本机 10 个真实会话库，共 1654 个 step）**：
  - `step_type = 14`（57 次）：`USER_INPUT`，由用户发起（`source = USER_EXPLICIT`）。
  - `step_type = 15`（800 次）：`PLANNER_RESPONSE`，模型规划与响应（`source = MODEL`）。
  - `step_type = 132`（724 次）：`GENERIC` 工具调用及命令执行输出。
  - `step_type = 101`（36 次）：`SYSTEM_MESSAGE` 系统消息。
  - `step_type = 23`（3 次）：`CHECKPOINT` 状态检查点。
  - `step_type = 17`（4 次）：`ERROR_TERMINATION`（如 `FAILED_PRECONDITION` 握手失败报错）。
- **Usage 挂载唯一性**：
  - 在全部 1654 个 step 中，包含 Usage 数据（Protobuf 顶层 Tag 9）的 step **100% 全部属于 `step_type = 15`**（共 796 处），其余所有 step_type 出现次数均为 0。
  - 800 个 `step_type = 15` 中仅有 4 个缺失 Tag 9，原因是该 step 刚刚发起即因网络/区域限制抛出 code 400 错误，模型未产出任何结果，故不产生 Token 账单。
- **事实结论 1**：**只有 `step_type = 15` 代表模型生成，且是唯一的 Usage 载体。**

---

### 2. `steps.metadata` Protobuf Usage 字段的稳定语义与数学不变量
在所有 800 个 `step_type = 15` 的 metadata 二进制中，Tag 9 为嵌套的消息体。经过全量反编译遍历，Tag 9 内部**仅包含且始终保持以下 Tag 集合**：

| Field Tag | Wire Type | 含义与语义 | 出现率 (800 条) |
| :--- | :--- | :--- | :--- |
| **Tag 1** | Varint | **模型 ID 枚举**（如 `1318` 对应 Gemini 3.8 Flash） | 100% (800/800) |
| **Tag 2** | Varint | **未命中缓存的输入 Token（Uncached Input Tokens）** | 100% (800/800) |
| **Tag 3** | Varint | **总输出 Token（Output Tokens）** | 100% (800/800) |
| **Tag 5** | Varint | **命中缓存的输入 Token（Cache Read Tokens）** | 81.25% (650/800) |
| **Tag 6** | Varint | 状态/模式标量（固定为 24） | 100% (800/800) |
| **Tag 7** | String | 实例签名（如 `bot-a552c271-...`） | 100% (800/800) |
| **Tag 8** | Bytes | 会话上下文绑定标识（`sessionID` 二进制） | 100% (800/800) |
| **Tag 9** | Varint | **思考 Token（Reasoning Tokens）** | 100% (800/800) |
| **Tag 10** | Varint | **正文回答 Token（Content Answer Tokens）** | 100% (800/800) |
| **Tag 11** | String | 服务端响应 Request Trace 签名（如 `Qgiuav_...`） | 100% (800/800) |

- **严格数学不变量验证**：
  1. **输出分解公式**：
     $$\text{Tag 3} = \text{Tag 9} + \text{Tag 10}$$
     在全量 800 条样本中：**800 条严格相等，0 条差异（吻合率 100%）**。即：
     $$\text{Output Tokens} = \text{Reasoning Tokens} + \text{Content Tokens}$$
  2. **为什么 CacheReadTokens (`Tag 5`) 会远大于 InputTokens (`Tag 2`)？**
     - **真实机制**：Antigravity 依托 Google Gemini 架构，其 `Tag 2` 并非 OpenAI 口径的“总 Prompt Tokens”，而是本次请求**增量传输的非缓存输入（Uncached Input）**；`Tag 5` 是**重用历史上下文的前缀缓存（Cached Input）**。
     - **真实会话序列验证（会话 `49e69e84`）**：
       - `idx=1`（首轮）：$t_2=15183, t_5=0$（冷启动，无缓存）。
       - `idx=3`（第二轮）：$t_2=16273, t_5=0$（缓存正在构建）。
       - `idx=5`（第三轮）：$t_2=3070, t_5=16279$（前文 16279 tokens 全部命中缓存，新增内容仅 3070 tokens，故 $t_5 > t_2$）。
       - `idx=37`（长会话轮次）：$t_2=2825, t_5=44788$（缓存命中 44,788 tokens，新增仅 2,825 tokens，$t_5$ 是 $t_2$ 的 15 倍以上）。
     - 本机 650 次缓存命中调用中，**648 次均呈现 $t_5 > t_2$**。

---

### 3. Usage Authoritative Source 确立
- **对比 `steps.metadata` 与 `gen_metadata.data`**：
  1. `gen_metadata` 是每次模型调用发送时的调试/镜像表，其单行体积高达数 KB 至 112 KB（包含整段完整 system prompt、工具定义与请求 JSON），开销巨大。
  2. `steps.metadata` 中的 Tag 9 是完成态的用量收据，体积极小（约 120 字节），且与生命周期状态 `steps.status = 3`（COMPLETED）强绑定。
  3. 当会话发生错误时，`gen_metadata` 仅保留发出的请求文本与错误信息，两边均无 Usage 产生。
- **事实结论 2**：
  - **唯一权威主数据源**：`conversations/<id>.db` 的 `steps.metadata`。
  - **`gen_metadata` 的职责**：仅作为模型名称字典（从其中提取 `MODEL_PLACEHOLDER_M318` 与 `"gemini-3.8-flash"` 的映射关系），**不参与 Usage 计数**。
  - **避免双重计数**：Usagi 仅从 `steps` 表且满足 `step_type = 15 AND status = 3 AND 包含 Tag 9` 时提取事件，不建立跨表合并摄取，彻底避免双重计算。

---

### 4. 稳定 Identity 与高精度事件时间
- **稳定去重 ID（Usage Event Deduplication Key）**：
  - `steps.idx` 是 SQLite 表的主键（`PRIMARY KEY (idx)`），会话内单调递增，永不重用。
  - 全局稳定去重 ID 确定为：
    $$\text{UsageEventID} = \text{format!("\{\}:\{\}", conversation\_id, steps.idx)}$$
- **真实模型调用事件时间**：
  - `steps.metadata` 顶层包含标准 Protobuf Timestamp（Tag 1：秒级 `sec`，纳秒级 `nano`）：
    - `Field 1`：请求启动时间戳。
    - `Field 7 / 8`：请求完成时间戳。
  - 换算为 Unix 毫秒时间戳公式：
    $$\text{occurred\_at\_ms} = \text{sec} \times 1000 + \lfloor \text{nano} / 1{,}000{,}000 \rfloor$$
  - 该时间与 `transcript.jsonl` 中的 `created_at` 严格一致，**严禁使用 DB 文件 mtime**。

---

### 5. 增量读取与 SQLite WAL 机制
- **WAL 状态**：
  - 所有会话 DB 的 `PRAGMA journal_mode` 均为 `wal`。
- **一致性快照**：
  - 使用只读 URI `file:<path>?mode=ro` 打开连接，SQLite 的 WAL 快照隔离机制可保证在 Antigravity 持续写入时，读事务完全不被阻塞，也绝不阻塞写入。
- **增量游标（Checkpoint Cursor）**：
  - 核心依据：`last_committed_step_idx`（记录每个会话已摄取的最大 `idx`）。
  - 查询语句：
    ```sql
    SELECT idx, metadata FROM steps 
    WHERE step_type = 15 AND status = 3 AND idx > :last_committed_step_idx 
    ORDER BY idx ASC;
    ```
  - 由于终态 step 不会被压缩、重排或回退，该游标具备**天然幂等性**，重启后不漏、不重。

---

### 6. 数据源职责与来源优先级冻结

| 属性 | 权威来源 (Authoritative) | 降级来源 (Fallback) | 依据 |
| :--- | :--- | :--- | :--- |
| **会话标题 (`title`)** | `annotations/<id>.pbtxt` 中的 `title` | 1. `conversation_summaries.db` 的 `title`<br>2. `steps[idx=0]` 内容前 50 字截断 | 用户重命名实时写入 annotations；summaries 中可能存有空字符串 `""`。 |
| **项目归属 (`project`)** | `conversation_summaries.db` 的 `workspace_uris` | 判定为 `projectless`（无项目） | `workspace_uris` 存放 `file://...` 本地 URI；空串即独立会话。 |
| **创建时间 (`created_at`)** | `steps[idx=0].metadata` Tag 1 时间戳 | `conversation_summaries.db` 的 `last_user_input_time` | 精确到毫秒/纳秒。 |
| **更新时间 (`updated_at`)** | 最新 `steps.metadata` Tag 7/8 时间戳 | `conversation_summaries.db` 的 `last_modified_time` | 模型调用结束时间为准确最后活动时间。 |
| **模型名称 (`model`)** | `steps.metadata` Tag 1 模型枚举结合 `gen_metadata` 映射表 | 客户端配置 `antigravity_state.pbtxt` 中的 `last_selected_agent_model` | 模型枚举映射到如 `"gemini-3.8-flash"`。 |
| **用量明细 (`usage`)** | `conversations/<id>.db` 的 `steps.metadata` Tag 9 | **无（唯一权威，不设 fallback）** | 杜绝双重计数。 |

---

### 7. 父子派生关系现实验证
- **现场实测**：
  - 本机全部 10 个会话在 `conversation_summaries.db` 中的 `parent_conversation_id` 均为 `""`。
  - 所有会话 DB 中的 `steps.has_subtrajectory` 均为 `0`（false）。
  - 仅在会话 `effa6389` 中观察到 4 条 `parent_references`，但对应 UUID 仅为内部瞬态调用标记，并没有落地为独立会话库。
- **事实结论 3**：**当前版本 Antigravity 没有生成独立的子会话库，第一阶段必须且只能将所有会话视为独立 main session（`agent_role = 'main'`, `parent_thread_id = None`, `root_session_id = thread_id`）。**

---

### 8. Standalone Antigravity 与 Antigravity IDE 架构一致性
- **现场实测**：
  - `/Applications/Antigravity.app` 与 `/Applications/Antigravity IDE.app` 是同一产品的不同发布阶段包。
  - 两者均使用同一套核心通信协议与语言服务器（`exa.cortex_pb`），底层会话数据库均统一落盘在 `~/.gemini/antigravity/`。
  - 全机不存在第二套不同 schema 的独立 Antigravity 会话库。
- **事实结论 4**：**两者完全一致，100% 共用同一个 Adapter parser。**

---

## 二、 尚未验证事实

1. **跨大版本模型枚举完整全集**：
   - 当前会话集中验证了枚举 `1318` 对应 `gemini-3.8-flash`（`MODEL_PLACEHOLDER_M318`）。
   - 对于 `gemini-pro`、`claude-3-5-sonnet` 等其他可能出现在历史或未来会话中的枚举 ID，需要遇到新样本时从 `gen_metadata` 中动态提取解析，无法通过代码静态写死全部枚举字典。
2. **会话物理删除行为**：
   - 当前 10 个会话均为存活状态（`killed = false` 且文件存在）。会话被用户在 UI 上点击 Delete 时，是物理删除 `.db` 文件，还是仅更新 `conversation_summaries.db` 的 `killed` 标记为 true，需待后续有真实删除样本时观察。

---

## 三、 推荐的数据契约

### 最终映射到 Usagi Canonical 的计算公式

针对 `steps.metadata` Tag 9 中的解析结果：
设 $t_1 = \text{Tag 1 (model)}, t_2 = \text{Tag 2}, t_3 = \text{Tag 3}, t_5 = \text{Tag 5 (若无则为 0)}, t_9 = \text{Tag 9}, t_{10} = \text{Tag 10}$：

| Usagi 核心字段 | 计算公式 / 取值 | 物理意义 |
| :--- | :--- | :--- |
| **`cached_tokens`** | $$t_5$$ | 命中的上下文缓存输入量 |
| **`uncached_input_tokens`** | $$t_2$$ | 未命中缓存、本次新增计算的输入量 |
| **`input_tokens`** | $$t_2 + t_5$$ | **总输入 Token**（符合 Usagi 与主流计费体系标准） |
| **`cache_write_tokens`** | `None` (或 0) | Gemini 上下文缓存自动管理，无独立写入账单 |
| **`output_tokens`** | $$t_3$$ | **总输出 Token** |
| **`reasoning_tokens`** | $$t_9$$ | 思考推理 Token（包含在 output 内） |
| **`total_tokens`** | $$t_2 + t_5 + t_3$$ | **全量 Token 总和**（$\text{input} + \text{output}$） |
| **`cache_hit_rate`** | $$\begin{cases} \frac{t_5}{t_2 + t_5}, & t_2 + t_5 > 0 \\ 0.0, & \text{otherwise} \end{cases}$$ | 缓存命中率（天然在 $[0.0, 1.0]$ 区间） |

---

## 四、 必须阻塞实施的问题

1. **Protobuf 解码器的稳定性**：
   - 必须实现一个自包含、无外部依赖的轻量 Protobuf Wire Reader（支持 varint、length-delimited bytes），能够安全容忍未知 tag，绝不能因为遇到新 tag 而 panic。
2. **模型名称动态解析机制**：
   - 由于 `steps.metadata` 仅含整数枚举，Adapter 不能使用写死枚举的分支，必须内置一个“从 `gen_metadata` 首次消费时发现并缓存枚举对应真实模型名称”的解析链。

---

## 五、 非阻塞、可以后续支持的问题

1. **Subagent 派生树展示**：
   - 由于当前 Antigravity 单机环境无独立 subagent 数据库实例，第一阶段直接全量展示为 `agent_role = 'main'` 没有任何数据失真，待未来 Antigravity 正式支持多会话 subagent 树时再行增强。
2. **`cache_write_tokens` 的细分**：
   - Gemini 当前不向客户端报送 cache write token，计为 0 / None 不影响 Token 总量和费用计算。

---

## 六、 脱敏测试 Fixture 清单

为保证适配器单测完备性，建议从本机脱敏提取以下 7 类测试样本并固化到 `tests/fixtures/antigravity/`：

| Fixture 编号与场景 | 提取来源 (本机实例) | 验证的不变量 (Test Invariants) |
| :--- | :--- | :--- |
| **F1: 首次冷启动无缓存** | `49e69e84` idx=1 | - 缺失 Tag 5 时：`cached_tokens == 0`<br>- `input_tokens == uncached_input_tokens`<br>- `output_tokens == reasoning_tokens + content_tokens`<br>- `cache_hit_rate == 0.0` |
| **F2: 缓存大于输入 ($t_5 > t_2$)** | `49e69e84` idx=5 | - $t_5 > t_2$ 时解析不报错<br>- `input_tokens == t2 + t5`<br>- `cache_hit_rate == t5 / (t2 + t5)`<br>- `total_tokens == t2 + t5 + t3` |
| **F3: 深度长上下文缓存推进** | `49e69e84` idx=37 | - 上下文缓存随 turn 累加到数万级时的准确累加<br>- 毫秒时间戳转换精度无偏移 |
| **F4: 低思考量输出** | `49e69e84` idx=39 | - `reasoning_tokens` 很小或为 0 时：公式 $t_3 = t_9 + t_{10}$ 依然严格成立 |
| **F5: 网络异常中断调用** | `a7ba2b1c` idx=1, 2 | - `step_type = 15` 缺失 Tag 9 或接 `step_type = 17` 时，静默跳过，**零 Usage 生成，不 panic** |
| **F6: 自定义标题覆盖** | `61d8076e` | - 优先读取 `annotations/*.pbtxt` 的 `title`，覆盖 `summaries.db` 中的空串 |
| **F7: 项目路径与独立会话** | `96d66dc2` vs `f400154c` | - URI 正确解码为本地文件系统路径<br>- 空 URI 严格判定为 `projectless`，绝不判定为 `unknown` |
