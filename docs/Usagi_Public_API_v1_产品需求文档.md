# Usagi Public API v1 产品需求文档

> 状态：Public API v1 产品契约
> 根路径：/api/v1

## 1. 目标

Public API v1 为运行在同一台计算机上的其他应用提供稳定、版本化、只读的数据访问能力。

首个明确使用场景是 PeekFlow：PeekFlow 通过 Mu 提供的 Public API 获取已经完成解析、持久化和聚合的 Codex Usage 与 Quota 数据，用于原生 macOS Widget 展示。

Public API v1 不是重新建设数据层，也不允许第三方应用直接读取 Mu SQLite 或 ~/.codex。Mu 继续负责 Codex 本地数据读取、Scanner / Parser、SQLite 持久化、Usage 聚合、费用估算、Quota 获取、状态与 revision 管理。

其他本地应用只消费 Public API v1 提供的产品化数据。

## 2. 架构定位

Mu 主链路：

~~~text
Codex 本地数据
    ↓
Scanner / Parser
    ↓
Mu SQLite
    ↓
Usage / Query / Aggregate Layer
    ↓
HTTP Layer
~~~

Public API v1 位于 Query / Aggregate Layer 之上，与 Mu 自用 Internal API 并列：

~~~text
Codex CLI / Codex Desktop
          ↓
   Session / Metadata
          ↓
     Scanner / Parser
          ↓
        SQLite
          ↓
 Usage / Query / Aggregate Layer
          ↓
        HTTP Layer
       /          \
      /            \
Internal API      Public API v1
  /api/*           /api/v1/*
     ↓                ↓
Mu Dashboard      PeekFlow / CLI / Future Local Apps
~~~

Public API v1 不直接读取 Codex 原始文件，不重新实现 SQL，不重新实现 Token 或费用聚合。

正确关系：

~~~text
                    ┌─ Internal API ── Mu Dashboard
Query/Aggregate ────┤
                    └─ Public API v1 ─ PeekFlow / Other Apps
~~~

Public API 与 Internal API 可以复用同一底层 Query / Aggregate 能力，但必须作为独立 HTTP 契约存在。Public API 不通过 HTTP 再调用 Internal API。

## 3. 与 Internal API 的关系

### 3.1 Internal API

路径：/api/*

Internal API 服务 Mu 自己的 Dashboard、桌面功能、更新与生命周期控制，可以包含查询、手动 refresh、service stop、update check、update open release 和 Mu 前端专用 DTO。

Internal API 不承诺作为第三方应用的长期稳定依赖。

### 3.2 Public API v1

路径：/api/v1/*

Public API v1：

- 面向其他本地应用；
- 首版只读；
- 形成版本化契约；
- 第三方应用可以长期依赖；
- Internal API 后续重构不得破坏 Public API v1；
- breaking change 必须通过未来 /api/v2/* 实现。

以下 Internal API 控制能力不进入 Public API v1：

~~~text
POST /api/refresh
POST /api/service/stop
POST /api/update/check
POST /api/update/open-release
~~~

## 4. 网络与安全边界

Public API v1 复用 Mu 当前 HTTP Server，并继续固定监听：

~~~text
127.0.0.1:3210
~~~

不得为了 Public API 改为 0.0.0.0。

“Public”表示对本机其他应用形成公开、稳定的 API 契约，不表示对局域网或互联网开放。

## 5. Public API v1 首版接口

| 数据 / 能力 | API 地址 | 提供的数据 | API 作用 |
| --- | --- | --- | --- |
| 服务信息 | GET /api/v1/info | Mu 应用版本、Public API 版本、capabilities | 服务发现与兼容性判断；确认当前连接的是 Mu，并判断客户端所需能力是否存在 |
| Revision | GET /api/v1/revision | data_revision、status_revision | 轻量判断 Usage 数据或 Mu 状态是否发生变化；用于启动校验、SSE 断线恢复和降级 polling |
| Revision SSE | GET /api/v1/events | SSE revision event，payload 为 data_revision、status_revision | 长连接变化通知；Mu 有变化时主动通知客户端，客户端再通过 GET 拉取真实数据 |
| Mu 状态 | GET /api/v1/status | 数据 revision、扫描状态、数据源状态、最近扫描结果/时间与安全错误码 | 告诉第三方应用 Mu 当前具体处于什么状态，用于 ready / updating / unavailable 等判断 |
| Codex Quota | GET /api/v1/codex/quota | Quota 状态、five_hour（5H）window、weekly window、使用/剩余比例、reset timestamp、抓取时间 | 用于 Widget 或其他应用展示 Codex 当前额度 |
| Usage 汇总 | GET /api/v1/usage/summary | Token、预估费用、Session 数、缓存命中率、Reasoning Token、数据完整状态和实际解析后的 range | Public API v1 的核心统计接口，用于 KPI、Widget、CLI 与其他汇总展示 |

首版不支持 Session 列表、Session 详情、模型分布、项目分布、Skills、模型/项目筛选、Filter Options、WebSocket 与任何 Public 写操作。

## 6. GET 与 SSE 的职责

Public API v1 明确采用：

- 真正的数据继续全部使用 GET；
- 变化通知使用 SSE；
- GET /revision 保留，作为 SSE 断开、启动恢复和降级 polling 的 fallback。

### 6.1 GET

真实数据继续通过：

~~~text
GET /api/v1/status
GET /api/v1/codex/quota
GET /api/v1/usage/summary
~~~

### 6.2 SSE

GET /api/v1/events 建立 Server-Sent Events 长连接。

SSE 不传完整 Usage 或 Quota，只发送 revision：

~~~text
event: revision
data: {"data_revision":183,"status_revision":42}
~~~

data_revision 变化后，客户端重新 GET /api/v1/usage/summary。

status_revision 变化后，客户端重新 GET /api/v1/status。

SSE 只承担 invalidation / notification，GET 继续承担 data fetching。

### 6.3 GET /revision 的保留意义

GET /revision 用于：

- 客户端启动时获取当前 revision；
- SSE 重连后校验是否错过变化；
- SSE 暂时不可用时降级 polling；
- 客户端主动进行一致性检查。

## 7. GET /api/v1/info

建议响应：

~~~json
{
  "service": "usagi",
  "app_version": "0.3.0",
  "api_version": "1",
  "capabilities": [
    "revision",
    "revision-events",
    "status",
    "codex-quota",
    "usage-summary"
  ]
}
~~~

客户端不应仅依赖 Mu 应用版本猜测 endpoint 是否存在。

## 8. GET /api/v1/revision

响应：

~~~json
{
  "data_revision": 183,
  "status_revision": 42
}
~~~

data_revision 表示 Mu 可查询 Usage 数据版本。变化时重新拉 Usage Summary。

status_revision 表示 Mu 数据/扫描状态版本。变化时重新拉 Status。

Codex Quota 当前独立于 Ledger revision。Public API v1 不增加 quota_revision；Quota 按客户端所需周期独立 GET。

## 9. GET /api/v1/events

Content-Type 为 text/event-stream。

事件名固定为 revision。

payload：

~~~json
{
  "data_revision": 183,
  "status_revision": 42
}
~~~

要求：

- 新连接建立后立即发送当前 revision；
- 后续 revision 变化时推送最新 tuple；
- 不积压完整历史事件；
- 允许发送 keepalive；
- 服务关闭时连接自然结束；
- 客户端负责断线重连；
- 重连后客户端可通过 GET /api/v1/revision 校验当前最新 revision。

## 10. GET /api/v1/status

Public Status 只暴露第三方应用需要的稳定状态，不暴露 target scan 等 Mu 内部控制细节。

首版包含：

- data_revision；
- status_revision；
- scan_state；
- source_binding_status；
- last_finished_scan_result；
- last_scan_started_at_ms；
- last_scan_completed_at_ms；
- last_scan_failed_at_ms；
- last_scan_error_code。

其职责是回答“Mu 现在是什么状态？”，而 /revision 回答“从上次读取后状态有没有变？”

## 11. GET /api/v1/codex/quota

Public Quota 首版提供：

- status；
- five_hour（5H）window；
- Weekly window；
- used_percent；
- remaining_percent；
- limit_window_seconds；
- reset_at_ms；
- fetched_at_ms。

Public API 返回绝对 reset timestamp，不返回 “in 14:29” 等 UI 文案。

Public Quota 不暴露 Codex account email、凭据、auth 内容、plan_type 或 reset credits。

## 12. GET /api/v1/usage/summary

Usage Summary 复用 Mu 当前 canonical Usage 口径，提供：

- input_tokens
- cached_tokens
- cache_write_tokens
- uncached_input_tokens
- output_tokens
- reasoning_tokens
- other_output_tokens
- total_tokens
- cache_hit_rate
- estimated_cost
- estimated_cost_status
- session_count
- cost_incomplete_session_count
- complete_session_cost_per_million_tokens
- session_health

响应同时返回 data_revision，以及实际解析后的 range.key、range.start_ms、range.end_ms、range.timezone。

Public API 不重新实现 Token 或费用计算。

## 13. 时间范围

Public API v1 支持：

| range | 语义 |
| --- | --- |
| today | Mu 本机时区今天 00:00 → 明天 00:00 |
| yesterday | Mu 本机时区昨天 00:00 → 今天 00:00 |
| 7d | 今天 + 前 6 个本地自然日 |
| 30d | 今天 + 前 29 个本地自然日 |
| year | 当前自然年 1 月 1 日 → 下一年 1 月 1 日 |
| custom | 调用方指定 from / to 日期范围 |

请求：

~~~text
GET /api/v1/usage/summary?range=today
GET /api/v1/usage/summary?range=yesterday
GET /api/v1/usage/summary?range=7d
GET /api/v1/usage/summary?range=30d
GET /api/v1/usage/summary?range=year
~~~

Public API 不把 today 命名为 24h，也不把 year 命名为 1y，因为自然日/自然年与 rolling window 语义不同。

## 14. 自定义日期范围

请求：

~~~text
GET /api/v1/usage/summary?range=custom&from=2026-09-01&to=2026-09-14
~~~

日期格式固定 YYYY-MM-DD。

from 与 to 均按 Mu 所在机器的本地时区解释。

to 为包含式自然日。例如 from=2026-09-01、to=2026-09-14 实际查询到 2026-09-15 00:00，因此 9 月 14 日全天包含在结果中。

## 15. 推荐客户端流程

初始化：

~~~text
GET /api/v1/info
GET /api/v1/revision
GET /api/v1/status
GET /api/v1/usage/summary?range=<selected-range>
GET /api/v1/codex/quota
连接 GET /api/v1/events
~~~

运行期间：

~~~text
SSE data_revision changed
        ↓
GET /api/v1/usage/summary

SSE status_revision changed
        ↓
GET /api/v1/status
~~~

Quota 按自身需要定时 GET。

SSE 断线：

~~~text
重新连接 /api/v1/events
        ↓
GET /api/v1/revision
        ↓
与本地保存 revision 对比
        ↓
必要时补拉 summary/status
~~~

## 16. 只读与数据最小化原则

Public API v1 首版全部为 GET。它是数据 Provider，不是 Mu Remote Control API。

Public API v1 不返回：

- Prompt 正文；
- Assistant 回复正文；
- rollout 原始 JSON；
- OpenAI/Codex 凭据；
- auth 内容；
- account email；
- SQLite 路径；
- SQL；
- Rust 原始错误；
- stack trace。

## 17. 错误模型

错误保持稳定的 HTTP status + JSON code：

~~~json
{
  "error": {
    "code": "INVALID_RANGE"
  }
}
~~~

错误响应不得包含文件路径、SQL、原始 Codex 内容或内部错误字符串。

## 18. 版本原则

Public API 主版本固定在 URL：/api/v1/*。

v1 内允许新增 endpoint、capability 和向后兼容的可选字段。

v1 内不允许删除已发布字段、修改字段类型、改变已有字段语义，或改变 today / yesterday / 7d / 30d / year / custom 的时间语义。

breaking change 必须使用未来 /api/v2/*。

## 19. 最终接口结构

~~~text
/api/v1
│
├── /info
├── /revision
├── /events
├── /status
├── /codex
│   └── /quota
└── /usage
    └── /summary
~~~

## 20. 验收标准

- [ ] Public API v1 位于现有 Mu HTTP Server 中，仍只监听 127.0.0.1:3210；
- [ ] /api/* Internal API 行为保持兼容；
- [ ] /api/v1/* 作为独立稳定契约存在；
- [ ] Public API 不通过 HTTP 调用 Internal API；
- [ ] GET /api/v1/info 返回服务、应用版本、API 版本与 capabilities；
- [ ] GET /api/v1/revision 返回当前 data/status revision；
- [ ] GET /api/v1/events 建立 SSE，并立即发送当前 revision；
- [ ] SSE 只推 revision，不推完整 Usage/Quota；
- [ ] GET /api/v1/revision 可用于 SSE 断线恢复与 fallback polling；
- [ ] GET /api/v1/status 不暴露 target scan 等内部控制细节；
- [ ] GET /api/v1/codex/quota 不返回 account email、plan_type、reset credits 或凭据；
- [ ] GET /api/v1/usage/summary 使用 Mu canonical Usage 口径；
- [ ] 时间范围支持 today、yesterday、7d、30d、year；
- [ ] 时间范围支持 custom + from + to；
- [ ] custom 按 Mu 本机时区解析且 to 为包含式自然日；
- [ ] 所有 Public API v1 endpoint 首版只读；
- [ ] Public API 不暴露 refresh、service stop、update control；
- [ ] Public API v1 有独立 contract tests；
- [ ] Internal API 后续重构不得静默破坏 Public API v1。
