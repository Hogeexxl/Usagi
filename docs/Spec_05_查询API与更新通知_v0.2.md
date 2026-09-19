# Usagi Spec 05：查询 API 与更新通知

> 版本：v0.2  
> 状态：当前契约修订版（Spec08 实施目标）  
> 更新日期：2026-08-09  
> 依赖：`Spec_01_数据模型和数据库骨架_v0.2.md`、`Spec_03_增量扫描器_v0.2.md`、`Spec_04_Token账本与聚合_v0.2.md`  
> 当前唯一测试标准：`Usagi_测试标准_Spec01-06_v0.17.md`

---

## 1. 范围

本 Spec 把稳定账本查询、扫描状态和手动刷新映射为仅本机可访问的 HTTP interface，并用 SSE 通知 revision 变化。Spec 06 不直接访问 SQLite、scanner 或 rollout 文件。

不实现 Dashboard 界面、WebSocket、远程访问、用户账户、价格表、查询缓存或新数据库表。`estimated_cost` 继续固定为 `null`。

---

## 2. 模块与路由

`QueryApi` 是 HTTP seam，内部只组合 `Ledger` 与 `ScanHandle`：

```text
AppContext {
  ledger: Arc<Ledger>
  scanner: ScanHandle
}
```

本 Spec 在 Spec 04 查询上定义快照 seam：

```text
UsageLedger::summary_snapshot(range) -> UsageSnapshot<UsageSummary>
UsageLedger::models_snapshot(range) -> UsageSnapshot<ModelUsageRows>
UsageLedger::sessions_snapshot(range, SessionPageRequest) -> SessionUsageSnapshot

SessionPageRequest = { limit, expected_data_revision?, after_sort_key? }
SessionUsageSnapshot = { data_revision, rows, next_sort_key? }
```

三个方法都在一个 SQLite 只读事务内冻结 active epoch 与 `data_revision`。Session 还必须在该事务内比较 `expected_data_revision` 并执行 keyset 查询。

`main.rs` 只负责创建依赖、组装 router、挂载静态文件并监听 `127.0.0.1:3210`。handler 不读取 SQL、不解析 rollout，也不等待整轮扫描。

固定路由：

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/api/health` | 进程存活检查，保持现有 `204` |
| GET | `/api/revision` | SSE 降级轮询 |
| GET | `/api/status` | 扫描与来源状态 |
| GET | `/api/usage/summary` | Dashboard 汇总 |
| GET | `/api/usage/sessions` | 根 Session 分页 |
| GET | `/api/usage/models` | 模型聚合 |
| POST | `/api/refresh` | 请求手动扫描 |
| GET | `/api/events` | SSE revision 通知 |

所有 `/api/*` 响应设置 `Cache-Control: no-store`。除 SSE 外使用 JSON；不得启用 wildcard CORS。在静态页面、API 和 SSE 之前先经过 3.10 的本机请求防护 middleware。

---

## 3. HTTP 契约

### 3.1 时间范围

三个 usage 查询都要求：

```text
range = today | yesterday | week | month | year
```

服务端在请求开始时冻结系统本地时区和当前时间，再转换为 UTC 毫秒 `[start_ms,end_ms)`：

| range | 本地开始 | 本地结束 |
|---|---|---|
| `today` | 今天 00:00 | 明天 00:00 |
| `yesterday` | 昨天 00:00 | 今天 00:00 |
| `week` | 本周一 00:00 | 下周一 00:00 |
| `month` | 本月 1 日 00:00 | 下月 1 日 00:00 |
| `year` | 本年 1 月 1 日 00:00 | 明年 1 月 1 日 00:00 |

实现使用支持系统 IANA 时区与 DST 的 Rust 时间库；禁止用 `24h` 秒数推算自然日，也禁止让前端传 UTC offset。本地边界是唯一 instant 时直接使用；overlap 固定选较早 instant；gap 固定选 gap 后第一个有效 instant。开始和结束以同一规则独立解析，整个 civil date 被跳过时允许 `start_ms=end_ms` 的零长范围。range 无效返回 `400 INVALID_RANGE`；无法取得系统 IANA 名称、tzdb 不可用或边界解析失败返回 `500 LOCAL_TIME_UNAVAILABLE`。

每个 usage 响应都带：

```json
{
  "range": {
    "key": "today",
    "start_ms": 0,
    "end_ms": 0,
    "timezone": "Asia/Shanghai"
  },
  "data_revision": 42
}
```

resolved range 在进入查询前只计算一次；active epoch、`data_revision` 和查询结果必须来自同一个 SQLite 只读快照。HTTP DTO 只做字段映射；Token 求和、nullable cache-write 传播、Session/root/model 口径全部调用 Spec 04 interface。

### 3.2 `GET /api/revision`

```json
{
  "data_revision": 42,
  "status_revision": 87
}
```

只读取 `Ledger::app_state()`，用于 SSE 断开后的 60 秒断线恢复轮询。

### 3.3 `GET /api/status?target_scan_id=<uuid>`

```json
{
  "data_revision": 42,
  "status_revision": 87,
  "scan_state": "idle",
  "active_scan_id": null,
  "last_finished_scan_id": "...",
  "last_finished_scan_result": "completed",
  "followup": null,
  "target_scan": null,
  "last_scan_started_at_ms": null,
  "last_scan_completed_at_ms": null,
  "last_scan_failed_at_ms": null,
  "last_scan_error_code": null,
  "source_binding_status": "ready"
}
```

Current Status API **不返回** `last_full_import_completed_at_ms`。该字段没有可靠生产写入/消费闭环，Spec08 从 current schema、Domain、API 和 Frontend 删除。首次导入或可用数据状态只能由 `scan_state`、`source_binding_status`、target/follow-up 以及 usage active/build epoch 等真实状态判断，不建立第二套“完整导入时间”投影。

`followup` 固定为 `null` 或：

```json
{
  "scan_id": "...",
  "state": "queued",
  "enqueued_status_revision": 88,
  "requested_at_ms": 0,
  "error_code": null
}
```

`state=start_failed` 时 `error_code` 必须是 Spec 01 的固定安全码；`queued` 时必须为 `null`。当 `scan_state=idle/failed` 但 `followup.state=queued` 时，仍表示有一次同步尚未启动，客户端不得把当前扫描终态当作 follow-up 终态。

`target_scan_id` 可选；未提供时 `target_scan=null`。提供时按 `scan_runs.scan_id` 主键返回：

```json
{
  "scan_id": "...",
  "state": "queued",
  "started_status_revision": null,
  "terminal_status_revision": null,
  "error_code": null
}
```

state 固定为 `queued|running|completed|failed|start_failed`。`SCAN_CANCELLED` 与 `SCAN_INTERRUPTED` 都表现为 `state=failed`，由 `error_code` 区分。合法 UUID 在 v1 找不到时返回 `404 SCAN_NOT_FOUND`；非法 UUID 返回 `400 INVALID_SCAN_ID`。app_meta 当前投影与 target row 必须由 `Ledger::scan_status_snapshot()` 在同一 SQLite 只读快照返回。v1 不清理 `scan_runs`，所以合法 target 终态不会被后续 scan 覆盖或变成 unknown。`last_finished_*` 只供界面展示，禁止用于 target 完成证明。

可空时间保持 `null`。错误只暴露 Spec 01 的结构化错误码，不返回 SQLite 文本、路径或异常堆栈。`running/failed/source_changed` 不清空旧稳定用量。

### 3.4 `GET /api/usage/summary?range=today`

响应：

```json
{
  "range": {},
  "data_revision": 42,
  "usage": {
    "input_tokens": 0,
    "cached_tokens": 0,
    "cache_write_tokens": 0,
    "uncached_input_tokens": 0,
    "output_tokens": 0,
    "reasoning_tokens": 0,
    "other_output_tokens": 0,
    "total_tokens": 0,
    "cache_hit_rate": null,
    "session_count": 0,
    "estimated_cost": null
  }
}
```

所有 usage 对象都使用固定 canonical Token DTO：`input_tokens`、`cached_tokens`、nullable `cache_write_tokens`、nullable `uncached_input_tokens`、`output_tokens`、`reasoning_tokens`、`other_output_tokens`、`total_tokens`、`cache_hit_rate` 和 `estimated_cost`；Summary 在此基础上增加 `session_count`。无匹配事件的对象固定为各整数 0、`cache_write_tokens=0`、`uncached_input_tokens=0`、`cache_hit_rate=null`、`estimated_cost=null`；范围内任一事件 cache-write 未知时只把 `cache_write_tokens` 与 `uncached_input_tokens` 返回为 `null`，不得猜测 0。整数必须位于 JSON 安全范围 `0..=2^53-1`，否则返回 `500 QUERY_OVERFLOW`。

### 3.5 `GET /api/usage/sessions`

参数：

```text
range     必填
limit     可选，默认 50，范围 1..=200
cursor    可选，只能使用上一页返回值
```

响应：

```json
{
  "range": {},
  "data_revision": 42,
  "items": [
    {
      "root_session_id": "...",
      "title": null,
      "project_name": null,
      "project_path": null,
      "last_activity_at_ms": 0,
      "models_used": ["gpt-5"],
      "subagent_count": 0,
      "inclusive_usage": {},
      "self_usage": {},
      "subagent_usage": {}
    }
  ],
  "next_cursor": null
}
```

排序固定为 `last_activity_at DESC, root_session_id ASC`。cursor 格式为 `v1.<base64url(payload)>.<base64url(HMAC-SHA256)>`，payload 包含 `data_revision`、`start_ms/end_ms`和末行排序键；HMAC 使用进程启动时生成的随机密钥并以常数时间比较。版本、编码、字段或认证失败返回 `400 INVALID_CURSOR`；进程重启后旧 cursor 因密钥失效也固定返回该错误。已认证 cursor 的 revision 或范围与当前请求不符返回 `409 STALE_CURSOR`，前端从第一页重取。

API 不返回 Subagent 独立行。三个 usage 对象复用 3.4 的固定 canonical Token DTO，不含 `session_count`；`models_used` 保留稳定顺序和 `unknown`。

### 3.6 `GET /api/usage/models?range=today`

```json
{
  "range": {},
  "data_revision": 42,
  "items": [
    {
      "model": "gpt-5",
      "usage": {},
      "session_count": 0,
      "first_activity_at_ms": 0,
      "last_activity_at_ms": 0
    }
  ]
}
```

默认排序为 `total_tokens DESC, model ASC`；`unknown` 是正常分组。空范围返回空数组。
`usage` 复用固定 canonical Token DTO；`session_count` 是模型行字段，不属于 `usage`。

### 3.7 `POST /api/refresh`

请求无 body，必须携带：

```text
X-Usagi-Request: 1
```

自定义 header 使其他网页的跨源简单请求不能静默触发扫描；服务端不启用 CORS。缺失或错误返回 `403 FORBIDDEN`。

handler 只 await `ScanHandle::request(Manual)` 返回服务端因果锚点，不等待后续文件 I/O 或整轮扫描：

```text
Started { scan_id, started_status_revision }
  -> 202 { "disposition":"started", "scan_id":"...", "status_revision":88 }

Coalesced { followup_scan_id, enqueued_status_revision }
  -> 200 { "disposition":"coalesced", "scan_id":"...", "status_revision":88 }
```

`status_revision` 不是 handler 另行查询 `app_state()` 拼接的快照。Started 响应中它是该 scan 的 started commit revision；Coalesced 响应中它是该 follow-up 首次排队 commit revision。Coalesced 的 `scan_id` 必须是 follow-up ID，不是当前 active ID；多个合并请求返回同一对 ID/revision。

scanner request 在 lifecycle/queue commit 前失败时不返回 Started/Coalesced；`SourceChanged` 映射 `409 SOURCE_CHANGED`，`Recovering` 与 `ShuttingDown` 都映射 `503 SCANNER_UNAVAILABLE`，started/enqueue commit busy 映射 `503 DATABASE_BUSY`，其他 started commit 映射 `500 SCAN_START_FAILED`，enqueue commit 映射 `500 SCAN_ENQUEUE_FAILED`。响应不暴露存储文本。

`source_binding_status=source_changed` 时返回 `409 SOURCE_CHANGED`，不请求扫描。连续点击最多合并一个 follow-up，互斥语义完全沿用 Spec 03。

### 3.8 `GET /api/events`

SSE 只发送 revision，不发送用量数据：

```text
event: revision
id: 87-42
data: {"data_revision":42,"status_revision":87}
```

实现要求：

1. `Ledger` 增加进程内 `subscribe_revisions()`，使用 Tokio `watch` 保存最新 `(data_revision,status_revision)`；它不是新的持久化事实。
2. Ledger 打开时从 `app_meta` 初始化；任何改变 revision 的事务只在 commit 成功后调用 `watch::Sender::send_replace`。即使当时无 receiver，当前值也必须更新且不影响提交。
3. 新连接立即发送 receiver 当前值；后续只在 revision tuple 变化时发送。慢客户端自然合并中间值，不建立无界队列。
4. receiver lag、SSE 重连或进程重启时，以数据库当前 tuple 为准；前端收到事件后重新请求 status 和当前 usage 页面。
5. 设置 `Content-Type: text/event-stream`、`Cache-Control: no-store`、`X-Accel-Buffering: no`。连接关闭只释放 receiver，不影响扫描。

浏览器 SSE 失败时，Spec 06 每 60 秒请求 `/api/revision`；发现任一 revision 变化后按需重取。

### 3.9 错误格式

```json
{
  "error": {
    "code": "INVALID_RANGE"
  }
}
```

固定映射：

| HTTP | code | 场景 |
|---:|---|---|
| 400 | `INVALID_RANGE` / `INVALID_LIMIT` / `INVALID_CURSOR` / `INVALID_SCAN_ID` | 请求参数错误 |
| 403 | `FORBIDDEN` / `FORBIDDEN_HOST` / `FORBIDDEN_ORIGIN` | refresh header 不匹配或本机请求防护失败 |
| 404 | `NOT_FOUND` / `SCAN_NOT_FOUND` | 未知 `/api/*` 路由或合法 scan ID 不存在 |
| 409 | `STALE_CURSOR` / `SOURCE_CHANGED` | 客户端状态冲突 |
| 503 | `DATABASE_BUSY` / `SCANNER_UNAVAILABLE` | SQLite busy timeout，或协调器仍在 lifecycle recovery/正在关闭 |
| 500 | `LOCAL_TIME_UNAVAILABLE` / `QUERY_OVERFLOW` / `QUERY_FAILED` / `SCAN_START_FAILED` / `SCAN_ENQUEUE_FAILED` / `INTERNAL_ERROR` | 本地时间不可解析、数值溢出、扫描开始/排队事务失败或其他内部失败 |

响应不得包含数据库 SQL、文件路径、原始错误字符串或正文。未知路由保持静态页面 fallback；未知 `/api/*` 必须返回 JSON `404 NOT_FOUND`，不能回退到 `index.html`。

### 3.10 本机请求防护

统一 middleware 只允许 `Host: 127.0.0.1:3210` 或 `localhost:3210`，其他或缺失 Host 返回 `403 FORBIDDEN_HOST`。有 `Origin` 时只允许与 Host 对应的 `http://127.0.0.1:3210` 或 `http://localhost:3210`，`Origin: null`也拒绝；`Sec-Fetch-Site: cross-site` 始终返回 `403 FORBIDDEN_ORIGIN`。无 Origin/Sec-Fetch 且 Host 合法的本机 CLI 允许访问。校验必须先于 health、静态 fallback、所有 API 和 SSE。

---

## 4. 实施步骤

### 步骤 1：建立查询模块

1. 新增 `src/api.rs` 与 `src/range.rs`；HTTP DTO 留在 api 模块，不污染 Token domain 类型。
2. 定义 `AppContext`，让 router、handler 测试注入同一 Ledger 与 ScanHandle；不为单个实现新增 trait。
3. 给 Cargo 增加 JSON 序列化、系统时区、Tokio watch、安全随机和 HMAC-SHA256 所需的最小依赖/feature；锁定版本并提交 lockfile。
4. 把现有 health 和静态文件 fallback 保留在同一个 router；`/api/*` 404 在静态 fallback 前截获。
5. 在整个 router 最外层安装 Host/Origin/Sec-Fetch middleware，失败时不进入任何 handler。

### 步骤 2：实现 range 与 DTO 映射

1. 把五个 range key 解析为枚举；每请求只读取一次 `now` 与本地时区。
2. 用日历运算得到本地边界，再转换 UTC ms；checked conversion，溢出返回 `INVALID_RANGE`。
3. 所有访问 Ledger 的 handler 都用 `spawn_blocking` 执行同步 SQLite 查询，不能阻塞 Tokio executor。
4. 实现第 2 节的三个 `UsageLedger` snapshot 方法；它们读取 active epoch/data revision 并调用 Spec 04 聚合，handler 不重算 Token。

### 步骤 3：实现分页与状态接口

1. API 先认证 cursor 并校验 range，再把 `expected_data_revision` 和 `after_sort_key` 传给 `sessions_snapshot`；不得把 SQL 片段放入 cursor。
2. `sessions_snapshot` 在同一只读事务中比较 expected/current revision，不符即 `STALE_CURSOR`；然后用 keyset SQL 取 `limit+1` 行。禁止先查 `app_state()` 再另开事务查页面。
3. `/api/revision` 通过 `Ledger::app_state()`；`/api/status` 验证可选 UUID 后调用 `Ledger::scan_status_snapshot(target)`，在同一快照返回当前投影与 target row，不另建内存历史或分两次查询。
4. 统一 `ApiError -> (StatusCode, Json<ErrorBody>)` 映射和 no-store header。

### 步骤 4：实现 refresh 与 SSE

1. refresh 先校验 header 和 source binding，再 await scanner request 的持久化 ack；Started 映射直接启动 scan ID/started revision，Coalesced 映射 follow-up ID/enqueue revision，禁止 handler 另查 app state 拼凑 revision。
2. Ledger 内建立 revision watch sender；所有增加 revision 的提交点在 commit 后调用同一个私有 publish 函数。
3. SSE 从 watch receiver 生成有界 stream；初始值、变化、断线与关闭均按 3.8 节处理。
4. 更新 README：列出本机地址、接口检查命令和 SSE/60 秒轮询关系，不记录用户数据示例。

---

## 5. 异常边界

- 首次导入或 rebuild 运行中：查询继续读取旧 active epoch；active=0 时返回合法零值。
- 扫描失败：status 变 failed，旧 usage 仍可查询；错误不改写 usage response。
- 请求期间 revision 改变：SQLite read transaction 返回开始时一致快照；下一次 SSE/轮询促使前端重取。
- 本地时区在请求间变化：新请求使用新时区；旧 session cursor 因范围不符失效。
- SSE 连接晚于 commit：watch 当前值仍包含最新 revision；不会依赖历史事件补发。
- SSE 慢客户端：只保留最新 tuple，不累积每次扫描事件。
- 数据库 busy：不重试长事务，不返回部分 JSON。
- 客户端断开：取消未开始的 blocking query；已进入 SQLite 的短只读查询允许结束并丢弃结果。
- `estimated_cost` 在所有层级只返回 `null`。

---

## 6. 测试方案

### 6.1 Range 与响应

- 五个 range 覆盖普通日期、月/年边界、midnight overlap/gap、整日跳过；overlap 选较早、gap 选后一有效 instant，跳日可为零长；
- 系统 IANA 名称缺失、tzdb 不可用和边界失败均返回 `LOCAL_TIME_UNAVAILABLE`；
- 缺失/未知 range、非法 limit/cursor 返回固定错误；
- summary 和 Session 空子 usage 的 canonical 0/null、比例和 estimated_cost JSON 类型正确；
- Session 排序、limit+1、next cursor、第二页、篡改/错版/cursor 重启失效、跨 revision/range cursor；
- model 按 total desc/model asc，unknown 保留；
- 每个 response 的 data revision、active epoch 和结果来自同一 read snapshot。

### 6.2 状态、刷新与通知

- idle/running/failed、last-finished ID/result、followup null/queued/start_failed、首次导入、source_changed 和所有可空时间正确映射；
- refresh header 缺失/错误为 403；Started 为 202 且返回直接 scan ID/started revision，running 时 Coalesced 为 200 且返回 follow-up ID/enqueue revision；handler 等待对应持久化 ack 但不等待扫描 I/O/完成；
- 当前终态早于 follow-up started 时 status 仍返回 queued；页面重载/网络中断后可从 status 恢复目标 ID；多个 Coalesced 共用同 ID；
- follow-up started Busy 时 target 继续保持 queued，并由协调器内部重试/重启恢复推进；只有非重试 internal、shutdown、source changed 才映射持久化 start_failed 安全码；
- status 已成功取得、binding ready、无 active/queued 时，前端从 idle 或 failed 发起 refresh 都能得到 Started 或固定错误；running/source_changed/queued 不会启动并发 scan；
- target T 完成后 F/G 又连续完成，`status?target_scan_id=T` 仍返回 T 终态；start_failed 后又开始新 scan 仍可查旧 target；
- 慢 SSE 跳过多个 revision、进程重启后 target 仍可查；status 当前投影和 target row 来自同一 read snapshot；
- started commit 失败不伪造 202；scanner recovering/shutdown/source changed 返回固定安全错误；
- source_changed 为 409 且 scanner 未收到请求；
- SSE 连接立即收到当前 tuple；data-only、status-only、两者同时变化各发一次最新值；
- commit 失败不 publish；无订阅者不影响 commit；慢 receiver 合并；断开后无任务/队列泄漏；
- SSE 断开后轮询 `/api/revision` 能发现相同 revision 变化。

### 6.3 HTTP、安全与恢复

- 服务只绑定 `127.0.0.1:3210`，不监听 `0.0.0.0`；
- 静态页、GET、SSE 和 refresh 均拒绝外部 Host、rebinding Origin 和 cross-site Sec-Fetch；合法 Host 的无 Origin CLI 可用；
- `/api/*` 无 wildcard CORS、no-store 生效，未知 API 返回 JSON 404，页面路由仍 fallback 到 index；
- SQLite busy、查询溢出和内部错误只返回安全 code，日志/响应无 SQL、路径、Prompt、回复或原始 JSONL；
- running/rebuild/failed 期间旧 stable usage 可读；active epoch 0 返回合法空结果；
- 并发查询与扫描不产生部分结果或写锁死锁；handler 的同步数据库工作不占 Tokio executor。

---

## 7. 独立验收标准

- [ ] 八个固定路由可用，未知 `/api/*` 不返回前端 HTML；
- [ ] 五个本地时间范围按唯一 overlap/gap/跳日规则转换 UTC，时区失败只返回固定服务端错误；
- [ ] summary、Session、model 完整映射 Spec 04，API 不重复实现聚合口径；
- [ ] Session cursor 经版本化 HMAC 认证，篡改或重启失效可预期；revision 校验与 keyset 查询位于同一快照；
- [ ] status 同时暴露 data/status revision、active/last-finished/follow-up 扫描状态、来源绑定和安全错误码；
- [ ] `status?target_scan_id` 在同一只读快照返回当前投影与持久化 target lifecycle；后续 scan、慢 SSE 或重启不覆盖 target 终态；
- [ ] refresh 只请求 Spec 03 Manual scan；Started 返回直接 scan ID/started revision，Coalesced 返回排队 follow-up ID/enqueue revision；只等待对应 ack 而不等待扫描完成，连续点击不并发扫描；
- [ ] 当前扫描终态到 follow-up 启动/启动失败全程可通过 status 确定追踪；Busy 始终保持 queued 并继续重试，只有非重试错误进入 start_failed；重载、断网和多次 Coalesced 不丢目标；
- [ ] failed 与 idle 在 binding ready、无 active/queued 时均可发起新 refresh；running/source_changed/queued 不会产生新并发扫描；
- [ ] SSE 首次发送当前 revision，后续有界合并通知；断线可由 `/api/revision` 轮询恢复；
- [ ] revision 只在数据库 commit 后 publish，失败事务不产生假通知；
- [ ] usage 查询在单个只读快照内绑定 active epoch、data revision 与结果；
- [ ] 扫描、首次导入或 rebuild 期间继续提供旧 stable 数据，失败不清空页面数据；
- [ ] 服务仅监听 loopback，不启用 wildcard CORS；Host/Origin/Sec-Fetch 防护覆盖静态页、全部 API、SSE 和 refresh；
- [ ] 错误响应和日志不泄露 SQL、路径、对话正文、工具内容或原始 JSONL；Session 正常响应只按正式字段返回根 Thread 项目元数据；
- [ ] `estimated_cost` 始终为 `null`；
- [ ] 未实现 Dashboard UI、WebSocket、远程访问、查询缓存或新持久化表。

---

## 8. 完成定义

通过本 Spec 后，Spec 06 只需根据 named range 调用 usage/status 接口，监听 revision 后重取数据；它不需要理解 SQLite、扫描互斥、Token 账本或 SSE 恢复细节。
