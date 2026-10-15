# Usagi KPI 会话 / 请求切换实施方案 v0.1

## 1. 文档目的

在同一批 KPI 记录上提供「会话 / 请求」展示切换。请求数和实测输出速度采用 Magpie 的统计规则。

## 2. 代码现状与问题

基线：`magpie-companion-spec05`，`e1404ee`，数据库 schema 3。

- `MetricGrid.tsx` 保留 `RequestMetric`，但当前网格显示会话数和账户额度。
- 当前 summary 来自会话投影的已筛选记录。旧 `usage` 包含其他消费者使用的统计。
- 数据库已有 TTFT、正文起点、流入时间和 Token。缺少 `handed_on`、`first_reasoning_ms`、`reasoning_hidden`。
- Magpie 规则参考本地 `参考项目/magpie/internal/usage/usage.go` 和 `ledger.go`。

## 3. 目标架构与范围边界

| 维度 | 纳入范围 | 排除范围 |
|---|---|---|
| 前端 | 筛选行右侧标准 pill Tabs；两张请求卡片；summary 新字段解析；会话列表每页 6 行及局部滚动修复；抽屉和列表 Token 缩写；Tool 图例对齐 | 图表数据、会话详情结构、托盘、其他卡片的外观与统计；会话排序规则 |
| 后端 | 原始字段同步、迁移和回填；同一投影记录的请求指标；summary DTO；额度读取中的短周期缓存轮询 | 新请求数据源、新请求列表、费用调整、修改 Magpie |
| 验证 | 迁移、同步、计数测速边界、接口和切换的定向测试 | 全仓测试、全量浏览器测试、其他 spec 的门禁 |

## 4. 架构契约与核心不变量

- [INV-01] 两个标签使用当前 KPI 的同一批筛选记录，不扩大到全部数据库请求。
- [INV-02] 旧 `usage` 字段与会话投影的既有结果保持不变。
- [INV-03] 默认标签为「会话」。切换不改变日期、筛选、图表和会话列表状态。
- [INV-04] 会话状态保留现有五卡；请求状态仅把会话数和账户额度替换为请求数和输出速度。
- [INV-05] 无有效速度样本时，速度卡只显示标题与 `—`，隐藏两行计时信息。

### 4.1 存储契约

schema 4 在 `requests` 增加以下列，保留其他列和记录：

| 列 | 类型 | 约束 |
|---|---|---|
| handed_on | INTEGER | NOT NULL DEFAULT 0 |
| first_reasoning_ms | INTEGER | NOT NULL DEFAULT 0 |
| reasoning_hidden | INTEGER | NOT NULL DEFAULT 0 |

历史日期使用现有日期同步机制回填。新增回填状态遵循现有 pending 模式；失败不清除 pending；冻结日期也应回填。

### 4.2 summary 接口契约

`/api/usage/summary` 及 `/api/usage/sessions` 的嵌套 summary 增加 `request_metrics`：

```ts
{
  calls: number;
  errors: number;
  retried: number;
  output_speed: number | null;
  speed_samples: number;
  mean_ttft_seconds: number | null;
  ttft_samples: number;
}
```

既有 `usage` DTO 不增加或替换字段。不从旧 `usage` 回退生成新卡片值。

## 5. 施工路线与任务分工

| 单元 | 独占写集 | 执行步骤 | 门禁 |
|---|---|---|---|
| 后端代理 | `internal/**` | 6.1–6.2、6.6 | 定向 Go 测试和接口契约 |
| 前端代理 | `frontend/**` | 6.3–6.6 | 定向 Vitest、TypeScript 检查和界面验证 |
| 主工程师 | 本文档 | 审核代码和验证证据 | 范围检查、统计规则和最终差异验收 |

代理使用 `gpt-6-luna`、`max`、`fork_turns: none`。完成任务即停止。禁止并发修改其他代理的写集。

## 6. 详细实施步骤

### 6.1 保存和回填缺失字段

扩充 Magpie 原始记录类型、数据库迁移、同步插入、查询扫描及内部行类型。沿用日期回填流程。测试旧记录保留、新字段落库、冻结日期回填及失败重试。

### 6.2 计算请求指标

在现有投影归属和合并路径内累加独立请求指标，保持 [INV-01] 和 [INV-02]。不新增全量请求查询路径。

- `handed_on` 不计 calls/errors/retried。失败采用 `status >= 400 || err != ""`。成功且 tries > 1 计 retried。
- 成功且 TTFT > 0 的记录累加 TTFT 与独立样本数。平均秒数为总毫秒 / 样本数 / 1000。
- 完整对齐 Magpie `Record.Decode` 和 `Row.Decode`：正文扣除 reasoning、可见推理起点、隐藏推理排除、100ms 最小窗口、10000 token/s 上限、flow 四分之一门槛、ThinkPace 20、log/Codex 的 1 秒和 100 Token 门槛。
- 整体速度为合格 Token 总和 × 1000 / 合格窗口毫秒总和。样本数为合格记录数。不混入 Floor 速度。

### 6.3 渲染标准标签和卡片

复用 `ui/beui/tabs` 的 `Tabs/TabsList/TabsTrigger`，使用 `variant="pill"`。在现有筛选行右对齐。状态仅保存在页面，不增加持久化设置。

复用 `RequestMetric` 的数字动画。移除提示图标、气泡及原速度页脚。卡片底部常驻两行：失败次数与成功率、重试后成功次数。完整失败短语使用 `text-destructive`，完整重试短语使用 `text-warning`。

请求数、失败数、重试数及两类计时次数在 100,000 以下显示不带分隔符的完整整数，达到阈值后使用现有一位小数缩写规则。用户已确认 `1K = 1,000`，例如 `141000 → 141K`。

新卡片显示标题「输出速度」、整数 `xx token/s`、`xxx 次计时`、底行 `首字平均 x.xs · xxx 次计时`。单位与数字使用同样的 28px 字号，单位使用常规字重并保留弱化色。数字滚动框与单位框在该卡片局部垂直居中对齐，不修改通用 NumberTicker。执行 [INV-05]。保持现有卡片高度和网格。

从现有一致性快照读取 `summary.request_metrics`，不新增轮询或 Magpie 请求。

### 6.4 缩短会话分页并移除表格内部纵向滚动

会话控制器及表格均使用每页 6 行。保留全量排序后分页、空值和同值排序、筛选重置及刷新后的页码收敛规则。仅在 Session 表格局部采用内容高度并禁止内部纵向滚动，保留横向滚动。初始加载显示 6 行骨架。通用 Table 及其他消费者保持不变。

### 6.5 统一 Token 显示并对齐 Tool 图例

抽屉的 Token 合计、各模型明细和 Compaction，以及列表总 Token 列复用 KPI 的 `formatIntegerFull` 缩写。合计动画使用相同 formatter。保留完整值的悬停标题和无障碍文本。比例、费用、请求数和计时次数保持各自现有格式。

将 Tool 加载失败提示放在标题右侧的同一行。提示不再占用标题与图例之间的独立行。内容区沿用相邻分布图的顶部间距；错误状态不下移图例。范围标签保持右对齐，窄屏提示可截断。

### 6.6 自动取得启动后的额度结果

Magpie 的额度缓存有效期为 1 分钟。前后端均在读取中等待 60 秒，会错过缓存完成窗口，且延长启动后卡片的等待时间。将前后端读取中轮询改为 5 秒。前端首次读取失败但尚无快照时也沿用读取中周期。取得完成快照后恢复原有正常轮询周期。

前端轮询只读 Usagi 缓存接口。后端读取不使用 `asked=true`，不循环触发手动刷新。保留请求互斥、手动刷新中止旧 GET、正常状态的轮询及卸载清理。

## 7. 测试与机器验收

- 后端只运行受改动链路的具名测试：迁移/回填、计数、测速、相同记录范围、旧字段不变和新 DTO。
- 前端只运行受改动的客户端/控制器/卡片/页面具名文件或用例，并运行 TypeScript 检查。
- 验收默认会话、请求五卡、筛选保持、样本数分离、无样本隐藏两行以及下方区域不变。
- 主工程师检查最终差异，不重复运行已通过且无后续改动的测试。不运行全量测试。

## 8. 交付与界面验收

打开 Dashboard，观察默认会话状态。切换请求，观察右侧两卡替换、已有三卡不变。切换筛选再切标签，观察筛选和下方内容保持。用零速度样本验证 [INV-05]。交付记录列明实际验证命令、结果及无法完成的验收项。

### 8.1 验收记录

2026-10-15，前后端实施代理和独立验收代理均使用 `gpt-6-luna`、`max`、`fork_turns: none`。主工程师检查最终差异及界面截图，未发现阻塞项。

通过的定向检查：

- 前端：`npm run check`。
- 前端：`npm test -- src/data/usagiClient.test.ts src/dashboard/useDashboardController.test.tsx src/dashboard/MetricGrid.test.tsx src/dashboard/DashboardPage.test.tsx`，4 个文件、46 个用例通过。
- 浏览器：`npm run test:browser -- --grep "KPI view toggle replaces the trailing cards and preserves date, filters, and page content"`，1 个用例通过。
- 数据库：`go test ./internal/database -run 'TestSchemaCreatesAndReopens|TestMigrationV1ToV4PreservesData|TestMigrationV3ToV4QueuesFrozenRequestMetricsBackfill|TestMigrationV2ToV4QueuesCachedCodexDaysIncludingFrozenDays'`。
- 同步：`go test ./internal/query -run 'TestSyncCompactionTriStateRoundTrip|TestSyncBackfillsFrozenCompactionDayRetriesAcrossReopenAndCompletesOnce'`。
- 投影：`go test ./internal/projection -run 'TestRequestMetrics|TestSummaryRequestMetricsIncludesUnknownAndLegacyRowsOnce|TestUsageAccumulator|TestSessionFieldsOutputSpeedAndRetries'`。
- 记录范围：上一命令中的范围用例名称未匹配，已单独执行正确命令 `go test ./internal/projection -run '^TestSummaryRequestMetricsIncludeUnknownAndLegacyRowsOnce$' -v` 并通过。
- 接口：`go test ./internal/api ./internal/magpie -run 'TestSessionConsistencyEmbedsListSummary|TestDatabaseReadEndpointsUseInitialSyncGateAndSessionDTO|TestUsageFingerprintIncludesTimingAndExcludesGeneratedFields|TestUsageRequestMetricFieldsJSON'`。
- 差异：`git diff --check`。

界面验收覆盖 1512px 和 390px 宽度。截图使用测试数据，不代表用户实时用量。未运行全量测试。未修改正在运行实例的数据库；更新后的程序启动时执行迁移，并通过正常同步回填历史字段。

### 8.2 UI 细节增补验收

用户确认标准 K 换算后，完成次数格式、常驻两行状态、语义色、单位字号及六行分页。

- `npm test -- src/dashboard/MetricGrid.test.tsx src/dashboard/format.test.ts`：40 个用例通过，1 个新增用例因标题断言选错节点失败。修正选择器后单独复跑该用例通过。
- `npx vitest run src/dashboard/session/useSessionTableController.test.tsx src/dashboard/session/SessionSection.test.tsx`：2 个文件、18 个用例通过。
- `npm run check`、`git diff --check`、`bash scripts/build_macos_runtime.sh` 均通过。
- 定向浏览器验收确认桌面和窄屏六行可见、全量排序后分页、横向滚动保留、六行加载骨架无纵向溢出、字号与状态文字正确。
- 已重启构建后的 Usagi。健康检查返回 204，状态为 ready，summary 返回请求指标。Magpie 桌面网关保持运行，临时 Vite 服务已停止。

### 8.3 字重、Token 格式、图例与启动额度验收

- 输出速度的现有定向用例通过。浅色截图确认单位字重为 400，数字和单位均为 28px，中心位置相同。
- Token 格式相关的 3 个测试文件、38 个用例通过。列表与抽屉均显示缩写，并保留完整数值。
- Tool 卡片的 3 个定向用例通过。浏览器确认正常和失败状态的图例顶部位置相同，错误提示位于标题行。
- 额度控制器的 5 个定向用例通过。后端通过额度快照、错误保留、读取完成及独立轮询周期的 4 项具名测试。
- 最终 `bash scripts/build_macos_runtime.sh` 通过，包含 TypeScript 编译。重新启动后未调用手动刷新接口，首次观测即取得 reading=false、Codex 2 个额度窗口和 Copilot 3 个额度窗口，均无错误。健康检查为 204，状态为 ready。
- 未运行全量测试。所有 UI 截图均为测试数据；实际启动验收只读接口状态，不输出账户身份。
