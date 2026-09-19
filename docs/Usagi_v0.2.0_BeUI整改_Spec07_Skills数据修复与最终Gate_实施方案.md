# Usagi v0.2.0 BeUI 整改实施方案 — Spec07：Skills 数据正确性修复 + 最终整体验收 Gate

> 施工分支：`feat/v0.2.0-beui-redesign`  
> 前置条件：**Spec06 / Gate S06 已通过。**  
> 本 Spec 第一部分修复 Skills invocation 漏报；第二部分执行 Spec01–07 最终整体验收。  
> 本文中的 Final Gate 即本轮最终 Gate，不再另立 Spec08。

---

# 1. Spec07 范围

## 1.1 Skills 数据正确性必须完成

1. 修复当前 Skill parser 召回率过低问题。
2. 保留 v0.1.3 的合法 `*_call` 兼容方向。
3. 不回退到 v0.1.3“递归扫描整个 payload 所有字符串”的误报方案。
4. 只扫描真实 tool-call 的参数树。
5. 不扫描普通 message / skills instructions / note metadata。
6. Skill locator 改成结构识别：
   - `.../skills/.../<skill_name>/SKILL.md`
7. **不枚举具体 Skills 安装目录**。
8. `skills` 必须是完整 path component。
9. `<skill_name>` = `SKILL.md` 直接父目录。
10. Windows `\` 与 Unix `/` 统一处理。
11. 保留同一次 call 内 Skill name 去重。
12. `USAGE_PARSER_VERSION`：
   - `9 → 10`
13. `SKILL_USAGE_PARSER_VERSION`：
   - `9 → 10`
14. `canonical_algorithm_for(9)` 必须继续有效。
15. parser 10 仍映射现有 canonical algorithm 5。
16. 通过既有 usage epoch rebuild 机制重建 `skill_usage_events`。
17. 禁止手改 SQLite `app_meta` 来伪造完成。
18. rebuild 完成后：
   - active parser = 10
   - build epoch 清空
   - Skills 7d API ready
19. Spec06 + Spec07 做一次联合真实数据验收。
20. 最后执行本文 Final Gate，复验 Spec01–07 整体页面与工程状态。

## 1.2 明确不做

- 不修改 Skill 排名 Top10 + Other 规则。
- 不修改 Skills rolling 7d。
- 不修改图表 Palette。
- 不修改 Skills Used UI；UI 已在 Spec06 固定。
- 不增加 Skill 文件系统扫描。
- 不访问实际 `SKILL.md` 文件内容。
- 不按 `.codex` / `.agents` / `.system` / `.curated` 枚举白名单。
- 不维护具体用户 home path 白名单。
- 不重新设计 usage canonical token algorithm。
- 不通过 blacklist Skill name 修复 parser。
- 不做 dual-read / fallback 到 parser 9。
- 不为了最终 Gate 发明尚未批准的“图标新规范”。

---

# 2. 当前代码问题与必须保留的架构

当前 `src/codex/skill_usage.rs`：

```text
只识别：
custom_tool_call
function_call
local_shell_call
```

并且：

```text
function_call → 只扫 command/cmd/path/file_path/skill_path
local_shell_call → 只扫 command/cmd
```

locator 又要求：

```text
.../skills/<skill>/SKILL.md
```

即 `skills` 必须直接位于 `<skill>` 前一级。

这比 v0.1.3 明显更窄。

v0.1.3 的有效点：

```text
payload.type.ends_with("_call")
```

但 v0.1.3 会递归扫描整个 payload，容易把无关字符串当 Skill。

### 本次最终目标

不是：

```text
v0.1.3 原样恢复
```

而是：

```text
合法 *_call 兼容
+
只扫描 call argument roots
+
递归扫描 argument tree
+
严格 Skill locator 结构证据
```

---

# 3. 文件范围

## 3.1 必须修改

```text
src/codex/skill_usage.rs
src/usage/normalized.rs
src/usage/analytics.rs
```

## 3.2 只有集成测试需要时允许修改

```text
src/usage/pipeline.rs
tests/spec04_usage_parser_rebuild.rs
tests/spec04_usage_integration.rs
```

`pipeline.rs` 原则上不需要改，因为当前已经在 owning item 上调用：

```text
collect_skill_events()
→ SkillUsageParser.parse_line()
```

如果 parser API 不变，不得顺手改 pipeline。

## 3.3 前端

Spec07 Skills 修复阶段原则上：

```text
frontend/** 不修改
```

Final Gate 如发现**明确违反 Spec01–06 已定规则**的残留，可以回到对应 Spec 所属文件做最小修正。

不得借 Final Gate重新设计 UI。

---

# 4. Parser v10 设计

# S07-1：保留快速预筛与 envelope 门槛

继续保留：

```text
line bytes 必须包含 "SKILL.md"
```

作为性能预筛。

继续要求：

```text
top-level type == "response_item"
payload 是 object
payload.type 存在
timestamp 可解析
```

普通：

```text
message
event
reasoning
output
instructions
```

不得进入 Skill detection。

---

# S07-2：call type 改为通用 `*_call`

删除当前：

```rust
match item_type {
    "custom_tool_call" => ...
    "function_call" => ...
    "local_shell_call" => ...
    _ => return None,
}
```

改为第一道门：

```rust
if !item_type.ends_with("_call") {
    return None;
}
```

这是 call envelope 兼容层。

但：

> `*_call` 只说明“可以继续检查参数”，不等于整个 payload 都可递归扫描。

---

# S07-3：只提取 call argument roots

新增一个明确 helper，例如：

```rust
fn collect_argument_roots(
    payload: &serde_json::Map<String, Value>,
    output: &mut BTreeSet<String>,
)
```

只读取真正承载 call 参数的 root：

```text
input
arguments
action
```

固定规则：

### `input`

如果存在：

```text
string / array / object
```

递归扫描其 value tree。

### `arguments`

如果是 JSON value：

```text
递归扫描
```

如果是 string：

1. 尝试 `serde_json::from_str::<Value>()`
2. 成功 → 递归扫描解析后的 argument tree
3. 失败 → 把该 string 本身作为 call argument text 扫描

这样 function arguments 不再局限：

```text
command
cmd
path
file_path
skill_path
```

固定叶子 key。

### `action`

如果存在：

```text
递归扫描整个 action tree
```

不再只看：

```text
command / cmd
```

### 不扫描

payload 其它字段例如：

```text
note
content
message
status
timestamp
type
id
call_id
```

不进入递归扫描。

### 新 call type

只要其参数放在：

```text
input / arguments / action
```

任一标准 argument root 中，就可以被识别，而无需给 `item_type` 继续加枚举。

---

# S07-4：参数树递归

统一 helper：

```rust
fn collect_locator_texts(
    value: &Value,
    output: &mut BTreeSet<String>,
)
```

行为：

```text
String → extract_from_locator_text
Array  → recurse each item
Object → recurse each value
Number/Bool/Null → ignore
```

注意：

这里只对 S07-3 选出的 argument root 调用。

禁止：

```text
collect_locator_texts(Value::Object(payload.clone()))
```

因为那会退回 v0.1.3 的整个 payload 扫描。

---

# S07-5：Skill locator 结构识别

现有：

```text
SKILL.md 前一层 = skill_name
再前一层必须 == skills
```

废止。

改为：

```text
SKILL.md 前一层 = skill_name
向更前方所有 path components 检查
只要存在完整 component == "skills"
→ 接受
```

## 步骤

输入 string：

```rust
let normalized = text.replace('\\', "/");
```

对每个 `SKILL.md` occurrence：

1. 取 `SKILL.md` 前方文本；
2. 去掉尾部 `/`；
3. 按 `/` 分 path components；
4. 最后一个非空 component = candidate `skill_name`；
5. `skill_name` 必须通过 `valid_skill_name()`；
6. `skill_name` 之前的祖先 components 中至少一个必须精确等于：

```text
skills
```

7. 满足则插入 `BTreeSet`。

## 应接受

```text
/Users/me/.codex/skills/frontend/SKILL.md
/project/.agents/skills/frontend/SKILL.md
/project/.agents/skills/.system/pdf/SKILL.md
/project/foo/skills/a/b/frontend/SKILL.md
C:\Users\me\.codex\skills\frontend\SKILL.md
```

以上只是结构样例，**不是目录白名单**。

## 不接受

```text
/tmp/frontend/SKILL.md
/foo/bar/SKILL.md
/tmp/my-skills-backup/frontend/SKILL.md
/tmp/skills-old/frontend/SKILL.md
const r = "SKILL.md"
```

### 关键定义

```text
skills
```

必须是完整 path component。

不得通过：

```text
contains("skills")
```

判断。

---

# S07-6：skill_name 校验

保留现有：

```text
非空
!= "."
!= ".."
length <= 128
无 control char
不含 / 或 \
```

不新增业务 name blacklist。

不得写：

```text
if skill_name == "const r" { reject }
```

false positive 必须由 locator 结构解决，不靠样例 blacklist。

---

# S07-7：同 call 去重

继续：

```text
BTreeSet<String>
```

同一 response_item call 中同一 Skill locator 出现多次：

```text
只生成 1 个 SkillUsageEvidence name
```

不同 call：

```text
分别计数
```

这与 `skill_usage_events` “调用次数”口径一致。

---

# S07-8：parser version 9 → 10

`src/usage/normalized.rs`：

```rust
pub const USAGE_PARSER_VERSION: i64 = 10;
```

### canonical mapping 必须同时修

当前：

```rust
6 | 7 | 8 | USAGE_PARSER_VERSION => Some(5)
```

当常量从 9 改成 10 后，如果不修改，parser 9 会从 mapping 中消失。

必须改为：

```rust
6 | 7 | 8 | 9 | USAGE_PARSER_VERSION
    => Some(USAGE_CANONICAL_ALGORITHM_VERSION)
```

即：

```text
parser 9 → canonical 5
parser 10 → canonical 5
```

原因：

本次只改变 Skill extraction，不改变 normalized token canonical algorithm。

禁止：

```text
canonical algorithm 5 → 6
```

---

# S07-9：Skills ready threshold 9 → 10

`src/usage/analytics.rs`：

```rust
pub const SKILL_USAGE_PARSER_VERSION: i64 = 10;
```

`skills_usage_snapshot()` 继续：

```text
ready =
active_epoch > 0
&& active_parser >= SKILL_USAGE_PARSER_VERSION
```

因此 rebuild 进行中、active 仍为 parser 9 时：

```text
ready = false
```

前端按已有协议显示 rebuilding / old-state fallback。

不得把 threshold 保持 9，否则新 parser build 尚未激活时 UI 会错误宣称 Skills 已 ready。

---

# S07-10：不得引入独立 Skill epoch

当前 Skill events 已经跟随 Usage epoch：

```text
UsagePipeline
→ skill_events
→ commit_usage
→ skill_usage_events(ledger_epoch)
```

本次继续使用现有 usage parser / epoch rebuild。

禁止新增：

```text
skill_active_epoch
skill_build_epoch
skill_parser_checkpoint
第二套 rebuild 状态机
```

这会过度设计。

---

# S07-11：Parser unit tests — 只保留必要 6 类

直接更新 `src/codex/skill_usage.rs` 现有 tests。

不建立几十个目录排列组合。

## P1：跨平台 + dedup

一个 call 同时包含：

```text
Unix direct
Windows direct
重复 Skill
```

结果：

```text
去重后正确名称
```

## P2：中间目录

至少一个：

```text
/project/.agents/skills/.system/pdf/SKILL.md
```

结果：

```text
pdf
```

证明不是强制：

```text
skills/<skill>/SKILL.md
```

直接相邻。

## P3：recursive argument tree

`function_call.arguments` JSON：

```json
{
  "nested": {
    "files": [
      "/x/skills/foo/SKILL.md"
    ]
  }
}
```

必须识别 `foo`。

证明不再只允许固定 leaf key。

## P4：generic `_call`

使用一个不在旧 3 类型中的：

```text
some_future_call
```

并在 `input` 或 `arguments` 放合法 locator。

必须识别。

## P5：false-positive 拒绝

同一测试覆盖：

```text
/foo/bar/SKILL.md
my-skills-backup/foo/SKILL.md
const r = "SKILL.md"
```

全部不识别。

## P6：unrelated payload 不扫描

合法 `function_call`：

```text
arguments 不含 Skill
note 含 /x/skills/false/SKILL.md
```

必须不识别。

另保留：

```text
message + skills instructions 不识别
missing timestamp 不识别
```

可与 P6 合并，不需要额外编号。

---

# S07-12：Rebuild 闭环

完成 parser + version 代码后，通过**正常应用启动 / scanner 流程**触发 parser mismatch rebuild。

禁止：

```sql
UPDATE app_meta ...
```

手工把 parser/version/epoch 改成 10。

## rebuild 前只读记录

至少记录：

```sql
SELECT
  usage_active_epoch,
  usage_parser_version,
  usage_build_epoch,
  usage_build_parser_version,
  data_revision
FROM app_meta
WHERE id=1;
```

以及当前 active epoch：

```sql
SELECT COUNT(*)
FROM skill_usage_events
WHERE ledger_epoch = (
  SELECT usage_active_epoch FROM app_meta WHERE id=1
);
```

只用于对比，不作为硬编码 expected count。

## rebuild 中

必须看到：

```text
active parser 仍可为 9
build parser = 10
```

不得提前切 active。

## rebuild 完成

必须：

```text
usage_parser_version = 10
usage_build_epoch IS NULL
usage_build_parser_version IS NULL
```

并且 active epoch 对应的 present sources 已按 parser10 完成 rebuild / 合法 carry。

---

# S07-13：重建后 `skill_usage_events` 数据核对

至少做三层核对。

## A. Fixture / parser truth

用 P1–P6 unit fixtures 确认：

```text
应识别的 locator → event
不应识别 → 无 event
```

## B. 真实 rollout 抽样

从当前真实 present rollout 中选取少量已知 Skill invocation 样本：

```text
5–10 个 call 即可
```

人工只读确认 raw call 中存在合法 locator，然后核对 active epoch：

```text
skill_usage_events
```

存在对应：

```text
skill_name
source_file_id
source_start_offset
source_end_offset
```

不要求大规模人工标注 400+ rollout。

## C. rolling 7d aggregate

对当前 7 个自然日：

```text
SUM(day.total)
```

必须等于相同 scope 下 active epoch `skill_usage_events` 的事件总数。

前端 Top10 + Other：

```text
只改变分组
不改变 total
```

---

# S07-14：Spec06 + Spec07 联合 Gate

在真实数据、parser10 active 后打开 Dashboard。

验证：

```text
Skills Used
```

必须：

1. 不再处于 rebuilding。
2. 7d total 与后端 7d aggregate 一致。
3. Skill 种类与 parser10 aggregate 一致。
4. Top10 正常独立展示。
5. 第 11 名以后进入 `其他`。
6. `其他` 仍计入 total。
7. 模型筛选生效。
8. 项目筛选生效。
9. Dashboard Range 切换不改变 rolling7d 周期定义。
10. Spec06 的 12px Axis / Legend、Project Folder palette、Gooey Popover 不回归。

这里只做一次真实联调，不为每种 filter 组合建立大量测试矩阵。

---

# 5. Spec07 最小测试标准

# T-S07-001：Parser v10 unit

P1–P6 全部 PASS。

重点：

```text
合法 *_call
argument tree recursion
ancestor component == skills
Windows
false-positive rejection
unrelated payload rejection
```

---

# T-S07-002：Version mapping

直接断言：

```text
USAGE_PARSER_VERSION == 10
SKILL_USAGE_PARSER_VERSION == 10
canonical_algorithm_for(9) == Some(5)
canonical_algorithm_for(10) == Some(5)
```

不增加新的 canonical algorithm version。

---

# T-S07-003：Rebuild 激活

使用现有 parser-rebuild integration 测试或最小新增 case：

```text
active parser 9
compiled target 10
→ build epoch 建立
→ build parser 10
→ 未完成前 active 仍 9
→ 完成后 active 10
→ build cleared
```

不重新测试整个 rebuild 状态机所有历史 edge cases。

---

# T-S07-004：Skill event replace

一个 source 在 parser9 active epoch 中有旧 Skill event。

parser10 rebuild 该 source 后：

- build epoch 中该 source 的旧 skill rows 被 source-level replace 清理；
- 新 parser10 events 写入；
- activate 后 aggregate 只读新 active epoch；
- 不出现同一 source 的 v9+v10 双计数。

只需 1 个 integration case。

---

# T-S07-005：真实 7d 联调

人工/只读数据验收：

```text
active parser = 10
build = NULL
Skills ready
API 7d total = DB scoped total
UI total = API total
```

并抽查 5–10 个真实 Skill invocation。

---

# 6. 必跑命令 — Spec07 功能 Gate

Rust：

```bash
cargo fmt --check
cargo test codex::skill_usage
```

再运行 parser/rebuild 相关现有测试：

```bash
cargo test --test spec04_usage_parser_rebuild
```

如果 Skill rebuild case 实际放在其它现有 integration target，使用实际 target，并在执行记录注明。

然后：

```bash
cargo test --test spec04_usage_integration
```

只要求包含与 `skill_usage_events / analytics` 直接相关的目标测试；如果该 target 无法过滤单 test，可运行整个 target。

Frontend Spec07 自身无新增代码时不单独增加 UI unit test；Spec06 已覆盖 Skills UI。

---

# 7. Gate S07 — Skills 功能 Gate

## Gate S07-A：Parser

- `*_call` gate。
- 只扫描 `input / arguments / action` 参数 root。
- 参数树递归。
- 不扫描整个 payload。
- 不扫描 message / note。
- 无具体 Skills directory 白名单。

## Gate S07-B：Locator

固定：

```text
.../skills/.../<skill>/SKILL.md
```

- `skills` = 完整 component。
- `<skill>` = `SKILL.md` 直接父目录。
- Windows / Unix。
- valid_skill_name 保留。
- 无 Skill name blacklist。

## Gate S07-C：Version

```text
USAGE_PARSER_VERSION = 10
SKILL_USAGE_PARSER_VERSION = 10
parser9 → canonical5
parser10 → canonical5
```

## Gate S07-D：Rebuild

- 正常 scanner 触发。
- 不手改 DB。
- active 未提前切换。
- 最终 active parser10。
- build cleared。
- skill events 无 v9/v10 双计数。

## Gate S07-E：真实数据

- 5–10 个真实 call 抽查正确。
- 7d DB/API/UI total 一致。
- 当前 Skill 种类由 parser10 真实数据产生。
- Top10 + Other 不丢 count。

S07-A～E 全 PASS 后才进入下面的 **Final Gate**。

---

# 8. Final Gate：Spec01–07 最终整体验收

> Final Gate 合并在 Spec07 内。  
> 它不新增设计需求，只验证 Spec01–07 已批准规则是否在整合后仍成立。

---

# F-1：BeUI 来源审计

对本轮涉及的 BeUI primitive 做一次最终来源核对：

```text
Button / StatefulButton
ActionSwap
ThemeToggle
AnimatedToastStack
Tabs
Checkbox
MorphPopover
TiltCard
NumberTicker
Popover
Table
Input
Tooltip
Drawer
BouncyAccordion
```

### 允许的本地差异只有已批准项

#### ThemeToggle

```text
next-themes → MU ThemeProvider bridge
```

#### Table

```text
manualSort
getRowProps
```

#### Drawer

```text
focus trap / first-focus
```

#### Checkbox

只有 Spec02 实际证明 label 破版后才允许：

```text
labelClassName passthrough
```

若未发生破版，不应存在该扩展。

#### 其它

只能是：

```text
import path mechanical adaptation
```

### FAIL 条件

发现任何：

```text
“看起来像 BeUI”
但无法对应当前官方 Registry / Manual 源码
```

直接 Final Gate FAIL。

---

# F-2：禁止残余扫描

只扫描 MU 业务层：

```text
frontend/src/dashboard/**
```

不要把 BeUI 官方内部参数误报为 MU 自定义。

必须确认没有本轮明确禁止的残余：

```text
text-[10px]
text-[11px]
CHART_FOCUS_TRANSITION
duration: 0.18  # 仅在 dashboard 自定义 Motion 中禁止
ThemeToggle start="top-right"
Table bg-card override
Drawer Refresh
Session ID Tooltip
Donut 152px
Skills used
自制日期 motion.div Popover
hash chart palette
```

注意：

> `frontend/src/ui/beui/**` 中官方源码本身存在 `0.18`、15px、14px icon 等值是允许的，不得为了“全局统一”擅自清理官方参数。

---

# F-3：全页 Desktop Dark 验收

使用实际数据，建议：

```text
1440px viewport
Dark
```

从上到下只做一次完整 smoke：

1. Header
2. 筛选
3. KPI
4. 模型分布 / 项目分布 / Skills Used
5. Session Table
6. 打开一个完整 Session Drawer
7. 展开 Main
8. 展开 Subagent

确认：

- 一级 section 32px。
- surface 层级一致。
- 没有透明 Drawer。
- 没有强黑框 Chart Card。
- 没有明显假 BeUI primitive。
- 没有布局跳动 / 横向滚动。
- NumberTicker 无异常二次闪烁。
- Gooey Popover / MorphPopover / Tooltip 均为标准视觉。

---

# F-4：全页 Desktop Light 验收

同一页面切 Light。

只验证主题相关：

- 首帧 / ThemeToggle 不回归；
- Chart palette 使用 Light 对应色；
- ChartSurface border 克制可见；
- Table / Drawer / Accordion surface 正常；
- Tooltip / Popover 对比度正常。

不重复所有业务交互。

---

# F-5：关键交互 smoke

最终只做以下必要交互：

1. Theme Dark ↔ Light。
2. 时间 Tabs 切换一次。
3. 模型筛选选 1 项并清除。
4. KPI Token Reasoning hover。
5. Distribution Token ↔ 费用。
6. Skills 日期 Gooey Popover。
7. Session sortable header 切换一次。
8. page2 → 输入3 → Enter → page3。
9. 打开 Drawer。
10. Main / Subagent 各展开 1 个。
11. Close Drawer，focus 返回。

不建立更大的手工矩阵。

---

# F-6：最终工程 Gate

## Frontend

```bash
cd frontend
npm run build
npm test
```

必须全部 PASS。

## Rust

仓库根目录：

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

必须全部 PASS。

禁止：

- 跳过失败 test；
- 放宽断言；
- `#[allow]` 掩盖本轮新增 warning；
- 用 test filter 隐藏 Final Gate 的其它回归。

Final Gate 是本轮唯一要求完整 Rust test / clippy 的阶段；前面各 Spec 保持 targeted tests。

---

# F-7：最终数据状态

最终只读确认：

```sql
SELECT
  usage_active_epoch,
  usage_parser_version,
  usage_build_epoch,
  usage_build_parser_version,
  data_revision
FROM app_meta
WHERE id=1;
```

必须：

```text
usage_parser_version = 10
usage_build_epoch = NULL
usage_build_parser_version = NULL
```

并确认：

```text
Skills API ready = true
```

不得在 build pending / blocked 状态宣布本轮完成。

---

# 9. Final Gate 判定

本轮 v0.2.0 BeUI 整改只有同时满足以下条件才算完成：

## Final-A

```text
Gate S01 PASS
Gate S02 PASS
Gate S03 PASS
Gate S04 PASS
Gate S05 PASS
Gate S06 PASS
Gate S07 PASS
```

## Final-B

官方 primitive 来源审计 PASS。

## Final-C

Dark 全页 smoke PASS。

## Final-D

Light 主题 smoke PASS。

## Final-E

关键交互 smoke PASS。

## Final-F

```text
frontend npm run build PASS
frontend npm test PASS
cargo fmt --check PASS
cargo test PASS
cargo clippy --all-targets -- -D warnings PASS
```

## Final-G

```text
usage parser 10 active
无 build pending
Skills ready
```

任何一项 FAIL：

> **本轮不允许标记完成。**

---

# 10. 施工员禁止事项

1. 禁止简单回退到 v0.1.3 Skill parser。
2. 禁止递归扫描整个 response payload。
3. 禁止维护 `.codex/.agents/.system/.curated` 目录白名单。
4. 禁止用 `contains("skills")` 判断目录组件。
5. 禁止用 Skill name blacklist 修 false-positive。
6. 禁止只改 `SKILL_USAGE_PARSER_VERSION` 不改 `USAGE_PARSER_VERSION`。
7. 禁止 bump parser10 后让 parser9 从 canonical mapping 消失。
8. 禁止 bump canonical algorithm version。
9. 禁止手改 SQLite app_meta 完成 rebuild。
10. 禁止保留 v9 fallback / dual-read。
11. 禁止只修前端 total 来掩盖漏报。
12. 禁止在 Spec07 重新设计 Skills UI。
13. 禁止 Final Gate 新增未讨论的 UI 规范。
14. 禁止把 BeUI 官方内部 0.18 / 15px 等参数当成 MU 残余清掉。
15. 禁止因为局部测试通过而跳过 Final Gate 全量 build/test/clippy。
16. 禁止以“整体看起来差不多”判完成；必须逐 Gate 有来源、有功能、有实际页面验收。
