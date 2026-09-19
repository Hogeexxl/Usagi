# Usagi v0.2.1 Codex Weekly Quota 实施方案

> 目标版本：**Usagi 0.2.1**  
> 施工对象：Luna 主控 + 2 个 Subagent  
> 基线：GitHub `main`，当前包版本 `0.2.0`  
> 范围：**仅 Codex Weekly Quota**。不扩展 Claude / 多供应商，不改现有 Token 统计口径，不改现有“同步数据”按钮语义。

---

# 1. 最终目标

在 Dashboard KPI 区最后新增一张 **CodexQuotaCard**，显示当前 Codex 登录账户的 Weekly 剩余配额。

最终卡片信息：

```text
┌─────────────────────────────┐
│ 剩余配额             Pro 5x │
│                             │
│ 45%                         │
│                             │
│ █████████░░░░░░░░░░         │
│                             │
│ 下次重置 · 08/12 12:23      │
└─────────────────────────────┘
```

右上角账户等级是 BeUI Popover Trigger，hover / focus / touch 可打开：

```text
hoge@example.com
重置卡：2 次
```

必须满足：

1. Weekly 数据来自 ChatGPT Codex 服务端，不从 MU rollout/token 数据推算。
2. 后端按 OpenUsage 方案识别 rate-limit window，前端当前只展示 Weekly。
3. Quota 服务独立于现有 Scanner。
4. Quota 远端刷新周期固定 **5 分钟**。
5. 现有右上角“同步数据”按钮与 Quota **完全无关**。
6. Quota 不写入 `mu.sqlite3`，不增加 migration。
7. Quota 不受 Dashboard 时间范围、模型筛选、项目筛选影响。
8. OAuth refresh 按 OpenUsage 已验证的策略实现，并允许 MU 在 refresh 成功后更新当前 `CODEX_HOME/auth.json`。
9. 最终完成 `0.2.1` 版本号更新、CI、Release workflow 和稳定版发布。

---

# 2. 硬性施工约束

## 2.1 禁止事项

本轮禁止：

- 新建多供应商 Provider 抽象。
- 引入 Claude 或其他供应商。
- 把 Quota 接入 `usage ledger`、Scanner、revision 数据链。
- 修改现有 Token / Cost / Session 数据口径。
- 修改 `/api/refresh` 的行为。
- 点击现有“同步数据”按钮时刷新 Quota。
- Quota 自动刷新时修改 `last_scan_completed_at_ms`。
- 为 Quota 新造 Card、Popover、NumberTicker primitive。
- 为 Quota 自定义新的色值、hex、主题 token。
- 修改现有公共 chart palette 色值。
- 为重置卡请求独立 `/rate-limit-reset-credits` endpoint。
- 实现“使用重置卡”。
- 实现 Spark limit。
- 在 UI 展示 5h Session limit。
- 为本功能新增数据库表。
- 在日志、错误信息、测试快照中输出 access token / refresh token / id token。
- 因本功能大范围重构现有 Dashboard。

## 2.2 必须复用

前端直接复用已经落地的 MU / BeUI：

- `TiltCard`
- `NumberTicker`
- `Popover`
- `PopoverTrigger`
- `PopoverContent`
- KPI 已有 `CARD / TITLE / VALUE / LEGEND` 视觉规则
- KPI 已有 5px rounded progress bar composition
- `motion/react` 现有 motion 体系
- `frontend/src/dashboard/charts/chartPalette.ts`
  - `chartSeriesColor(index)`
  - `chartMuted`

不得复制一套近似实现。

---

# 3. 数据来源和固定口径

## 3.1 Credential 来源

本版本只使用当前 MU 已解析的：

```text
CODEX_HOME
```

Credential 文件：

```text
<CODEX_HOME>/auth.json
```

不得再建立第二套 Codex Home 发现逻辑。

v0.2.1 不实现 Codex 可选的 keyring / ephemeral credential store。当前 Codex 默认 CLI auth store 是 file；如果用户主动把 Codex 改成仅 keyring / ephemeral，Quota 返回不可用，但 MU 其余功能必须正常。

## 3.2 auth.json 读取字段

至少读取：

```text
tokens.access_token
tokens.refresh_token
tokens.id_token
tokens.account_id
last_refresh
OPENAI_API_KEY
```

刷新写回时必须：

- 更新对应 token；
- 更新 `last_refresh`；
- **保留 JSON 中所有未知字段**；
- 不得把 auth.json 反序列化后只写回 MU 已知字段，从而删除 Codex 后续新增字段。

账户邮箱：

```text
tokens.id_token JWT payload -> email
```

账户真正请求标识：

```text
tokens.account_id
```

如 `tokens.account_id` 缺失，可从 `id_token`：

```text
https://api.openai.com/auth.chatgpt_account_id
```

读取 fallback account id。

前端只显示 email，不显示 account_id。

## 3.3 Usage endpoint

固定：

```text
GET https://chatgpt.com/backend-api/wham/usage
```

Headers：

```text
Authorization: Bearer <access_token>
Accept: application/json
User-Agent: Usagi/<current package version>
ChatGPT-Account-Id: <account_id>   # 有值时才发送
```

请求 timeout：

```text
10 seconds
```

## 3.4 OAuth refresh endpoint

固定：

```text
POST https://auth.openai.com/oauth/token
```

Content-Type：

```text
application/x-www-form-urlencoded
```

Body：

```text
grant_type=refresh_token
client_id=app_EMoamEEZ73f0CkXaXp7hrann
refresh_token=<current refresh_token>
```

请求 timeout：

```text
15 seconds
```

---

# 4. OAuth refresh：按 OpenUsage 可靠机制实现

这一部分禁止 Luna 自行简化成“401 就报错”。

## 4.1 access token 过期判断

优先解码 access token JWT payload 的：

```text
exp
```

固定提前刷新窗口：

```text
5 分钟
```

即：

```text
expires_at - now <= 5min
=> needs_refresh
```

为解码 JWT payload，可新增纯 Rust `base64` 直接依赖；禁止引入重量级 JWT 验签框架。本功能只读取本机 Codex token 的 payload 元数据，不做 token 签名认证。

如果 JWT 无法解码 `exp`：

- 有 `last_refresh`：仅作为兼容 fallback，超过 8 天视为需要 refresh；
- 没有 `last_refresh`：不要强制 refresh，直接先尝试当前 access token。

## 4.2 refresh 前必须重新读取同一个 auth.json

流程：

```text
第一次读取 auth.json
        ↓
发现 access token 接近过期
        ↓
重新读取同一个 <CODEX_HOME>/auth.json
        ↓
若 Codex CLI 已经旋转出新 token
        ↓
采用磁盘上的新 token
        ↓
重新判断 needs_refresh
```

目的：避免 MU 使用已经被 Codex CLI 消耗/旋转过的旧 refresh token，触发：

```text
refresh_token_reused
```

## 4.3 仍需 refresh 时

若重新读取后仍需要 refresh，且有 refresh token：

```text
POST /oauth/token
```

成功响应：

```text
access_token  必须存在且非空
refresh_token 可选
id_token      可选
```

写回规则：

- `access_token`：必更新；
- response 有新的 `refresh_token`：覆盖旧值；
- response 有新的 `id_token`：覆盖旧值；
- response 没返回 refresh/id token：保留旧值；
- 更新 `last_refresh` 为当前 ISO-8601 时间；
- 保存回原 `auth.json`。

保存失败：

- 不使本次已经成功拿到的新 token 失效；
- 本次进程继续使用内存中的新 token；
- 记录不含任何 token 内容的错误；
- 下一轮再次按正常流程读取/处理。

## 4.4 usage 请求的认证重试

第一次 `/wham/usage`：

- 2xx：进入 mapper。
- 401 / 403：允许 **一次** refresh + retry。
- 其他 4xx / 5xx：不无限重试。
- 网络错误/timeout：失败。

401 / 403 retry：

```text
当前 refresh_token
  ↓
refresh OAuth
  ↓
写回 auth.json
  ↓
用新 access token 再请求 /wham/usage 一次
```

最多一次认证重试，禁止循环 refresh。

## 4.5 refresh error 分类

至少识别：

```text
refresh_token_expired
refresh_token_reused
refresh_token_invalidated
```

这些都视为需要用户重新登录 Codex：

```text
auth_required
```

如果只有 `OPENAI_API_KEY`、没有 OAuth access token：

```text
auth_required
```

API Key 不用于 Subscription Weekly Quota。

---

# 5. Weekly Mapper

建议新增：

```text
src/codex/quota/
  mod.rs
  auth.rs
  client.rs
  mapper.rs
```

并在：

```text
src/codex/mod.rs
```

注册：

```rust
pub mod quota;
```

## 5.1 Window 分类

解析：

```text
rate_limit.primary_window
rate_limit.secondary_window
```

每个 window 读取：

```text
used_percent
limit_window_seconds
reset_at
reset_after_seconds
```

分类固定：

```text
18000 seconds  -> Session
604800 seconds -> Weekly
```

算法必须和 OpenUsage 的核心规则一致：

1. 先按 `limit_window_seconds` 精确分类。
2. `primary_window` 如果 duration=604800，必须识别成 Weekly。
3. `secondary_window` 如果 duration=18000，必须识别成 Session。
4. 只有 duration 缺失或是未知 duration 时，才允许位置 fallback：
   - primary -> Session
   - secondary -> Weekly
5. 前端本版本只消费 Weekly。
6. Session 即使后端识别到了，也不进入当前 QuotaCard DTO。

## 5.2 used_percent

优先：

```text
window.used_percent
```

缺失时允许使用对应 response header fallback：

```text
x-codex-primary-used-percent
x-codex-secondary-used-percent
```

有效范围：

```text
0 <= used_percent <= 100
```

超出范围视为 payload 无效，不得 clamp 成正常数据。

计算：

```text
remaining_percent = 100 - used_percent
```

后端保存原始小数；前端主数字显示整数百分比。

## 5.3 reset time

优先：

```text
reset_at
```

服务器值是 Unix seconds，转换为：

```text
reset_at_ms
```

若没有 `reset_at`：

```text
now + reset_after_seconds
```

若两者都没有：

```text
reset_at_ms = null
```

前端 null 时显示：

```text
下次重置 · —
```

## 5.4 plan_type

从 `/wham/usage` 原样保存：

```text
plan_type
```

后端不映射，不封闭 enum。

例如：

```text
free
plus
prolite
pro
```

以及未来未知值都允许保留。

## 5.5 reset credits

只从本次 `/wham/usage` body：

```text
rate_limit_reset_credits.available_count
```

读取。

规则：

- 非负数字有效；
- 取 floor 后转整数；
- `0` 是真实值；
- 缺失 / null / 非数字 -> `null`。

本版本不请求：

```text
/backend-api/wham/rate-limit-reset-credits
```

因为 UI 只需要“剩余次数”，不需要每张卡的过期时间。

---

# 6. 后端 Quota API Contract

固定 endpoint：

```text
GET /api/codex/quota
```

不接受：

```text
range
model
project
expected_data_revision
```

响应结构固定：

```json
{
  "status": "ready",
  "account_email": "hoge@example.com",
  "plan_type": "prolite",
  "weekly": {
    "used_percent": 55.0,
    "remaining_percent": 45.0,
    "limit_window_seconds": 604800,
    "reset_at_ms": 1786508580000
  },
  "reset_credits_available": 2,
  "fetched_at_ms": 1786076580000
}
```

`status` 只允许：

```text
loading
ready
auth_required
unavailable
```

字段规则：

### ready

```text
weekly != null
```

其他字段可按真实返回为 null。

### loading

第一次后台请求尚未完成：

```text
account_email = null
plan_type = null
weekly = null
reset_credits_available = null
fetched_at_ms = null
```

### auth_required

没有可用 OAuth credential / refresh token 已失效，需要重新登录 Codex。

无 last-good 时：

```text
weekly = null
```

### unavailable

网络失败、server error、invalid payload 等，且没有任何 last-good。

无 last-good 时：

```text
weekly = null
```

## 6.1 last-good 规则

只要本进程内曾经成功获取过 `ready`：

后续某一轮：

```text
network error
timeout
5xx
invalid payload
```

不得把已经显示的 Weekly 清空。

继续返回最后一个 `ready` snapshot，`fetched_at_ms` 保持最后成功时间。

本版本不额外向前端暴露 `stale` 字段，不扩大 UI 状态。

---

# 7. CodexQuotaService

## 7.1 架构

新增独立内存服务：

```text
CodexQuotaService
```

职责：

```text
读取 credential
OAuth refresh
请求 wham/usage
mapper
维护 last-good snapshot
5 分钟自动刷新
提供当前 snapshot 给 HTTP API
```

禁止持有：

```text
Ledger lock
Scanner lock
SQLite connection
```

远端请求期间不能占用 MU 现有数据链资源。

## 7.2 刷新周期

固定：

```text
300 seconds
```

生命周期：

```text
MU 服务启动
  ↓
HTTP 服务可用
  ↓
spawn CodexQuotaService background task
  ↓
立即第一次 fetch
  ↓
第一次完成后 + 5min
  ↓
下一次 fetch
```

timer 使用 delay 语义：

- 如果某次请求耗时，不补跑错过的 ticks；
- 下一周期从本次完成后继续；
- 禁止并发执行两次 Quota fetch。

## 7.3 与 Scanner 完全隔离

以下动作不得触发 Quota：

```text
POST /api/refresh
右上角同步按钮
Scanner scheduled scan
Scanner startup scan
```

以下动作也不得触发 Scanner：

```text
Quota background refresh
GET /api/codex/quota
```

Quota 更新不得修改：

```text
data_revision
status_revision
last_scan_started_at_ms
last_scan_completed_at_ms
```

## 7.4 AppContext

在：

```text
src/api.rs
```

`AppContext` 增加：

```text
codex_quota_service: Arc<CodexQuotaService>
```

新增：

```text
.route("/codex/quota", get(codex_quota))
```

注意整个 API 已 nest 在 `/api`，最终地址是：

```text
/api/codex/quota
```

Handler 只读取内存 snapshot，不进行 SQL，不调用 Scanner。

## 7.5 main.rs

启动阶段：

1. 按现有逻辑完成 listener / ledger / scanner。
2. 构造 `CodexQuotaService`，传入 `ledger.codex_home()`。
3. 将 service 注入 `AppContext`。
4. HTTP 服务 ready 后，spawn quota background task。
5. Quota 第一次网络请求不得阻塞 HTTP server ready。
6. shutdown 时 abort 并 await quota task，和现有 update background task同级处理。

如果 Quota HTTP client 构造失败：

- 生成 unavailable service；
- MU 主程序仍正常启动。

---

# 8. 前端 Contract 与 Controller

## 8.1 types.ts

新增：

```ts
export type CodexWeeklyQuotaDto = {
  used_percent: number;
  remaining_percent: number;
  limit_window_seconds: number;
  reset_at_ms: number | null;
};

export type CodexQuotaResponse = {
  status: "loading" | "ready" | "auth_required" | "unavailable";
  account_email: string | null;
  plan_type: string | null;
  weekly: CodexWeeklyQuotaDto | null;
  reset_credits_available: number | null;
  fetched_at_ms: number | null;
};
```

## 8.2 usagiClient.ts

`UsagiClient` 新增：

```ts
codexQuota(signal?: AbortSignal): Promise<CodexQuotaResponse>;
```

请求：

```text
GET /api/codex/quota
```

Parser 必须验证：

- `status` enum；
- percent finite 且 0..100；
- `limit_window_seconds === 604800`；
- `reset_at_ms` nullable safe integer；
- reset credits nullable safe integer；
- `ready` 时 weekly 必须非 null；
- 非 ready 且无 last-good 的响应 weekly 为 null。

不把 Quota 加进 dashboard query key。

## 8.3 useCodexQuotaController.ts

新增独立 controller：

```text
frontend/src/dashboard/useCodexQuotaController.ts
```

行为：

1. 页面 mount 立即 GET 本地 `/api/codex/quota`。
2. 如果返回 `loading`，1 秒后重试；只用于等待第一次后台 fetch，不触发远端 Quota fetch。
3. 第一次非 loading 结果后，前端每 5 分钟重新读取一次本地 Quota API。
4. 页面 unmount abort 当前 request 并清理 timer。
5. 不监听 Scanner revision feed。
6. 不调用 `request_refresh`。
7. 不因为 range / model / project 改变重新请求。
8. 本地 API 临时请求失败且已有 ready view 时，保留现有 view。

---

# 9. plan_type 前端显示

新增纯 formatter，建议放在：

```text
frontend/src/dashboard/format.ts
```

或与 Quota controller/card 同文件的纯函数；优先避免新增无必要文件。

固定映射，参考 OpenUsage：

```text
prolite -> Pro 5x
pro     -> Pro 20x
```

其他非空字符串：

- 按 `_` 分词；
- Title Case；
- 因此：
  - `plus` -> `Plus`
  - `free` -> `Free`
- 未知的新 plan 仍能显示，不报错。

null：

```text
—
```

后端仍保留服务器原始 `plan_type`。

---

# 10. CodexQuotaCard

内部组件名固定：

```text
CodexQuotaCard
```

优先直接放入：

```text
frontend/src/dashboard/MetricGrid.tsx
```

原因：

- 直接复用该文件已经存在的 `CARD / TITLE / VALUE / LEGEND`；
- 直接复用已有 `CompactTicker`；
- 避免为了这一张卡复制 KPI 样式；
- 避免新建第二套 Metric Card composition。

不要为了拆文件而复制视觉常量。

## 10.1 卡片容器

直接：

```text
TiltCard
```

尺寸和 Cache / Session / Cost 卡一致：

```text
height = h-36
desktop fixed column = 236px
```

标题：

```text
剩余配额
```

主值：

```text
remaining_percent
```

显示整数：

```text
45%
```

主值必须使用：

```text
NumberTicker
```

不得显示：

```text
used_percent
```

作为主值。

## 10.2 右上角账户等级

标题行结构和现有“预估费用 + 右侧状态”一致：

```text
left  = 剩余配额
right = plan display
```

例如：

```text
Plus
Pro 5x
Pro 20x
```

右侧等级本身作为：

```text
PopoverTrigger
```

使用现有 BeUI：

```tsx
<Popover trigger="hover" side="bottom" align="end">
```

不要使用 Info icon。

不要在 Popover 里重复显示 plan。

## 10.3 Popover 内容

固定只有两行：

```text
hoge@example.com
重置卡：2 次
```

禁止加入 label：

```text
账户：
邮箱：
账户等级：
```

缺失值：

```text
email missing         -> —
reset credits missing -> 重置卡：—
```

使用 BeUI `PopoverContent` 现有尺寸/文字规则，只做必要 flex/gap 排版。

## 10.4 Progress bar

复用当前 KPI 进度条 composition：

```text
relative
mt-2
h-[5px]
overflow-hidden
rounded-full
```

不得新造 Progress primitive。

条含义：

```text
remaining 部分 = 关键颜色
used 部分      = chartMuted
```

长度：

```text
remaining width = remaining_percent%
```

## 10.5 公共色盘

已确认 MU 当前图表统一通过：

```text
frontend/src/dashboard/charts/chartPalette.ts
```

使用：

```text
chartSeriesColor(index)
chartMuted
```

Quota **必须直接使用同一个公共 palette API**。

不得在 QuotaCard 写：

```text
#3fbe95
#ffd10a
#e03e4c
```

固定 palette slot：

```text
绿色 -> chartSeriesColor(8)  // 当前 series-9
黄色 -> chartSeriesColor(5)  // 当前 series-6
红色 -> chartSeriesColor(9)  // 当前 series-10
used -> chartMuted
```

颜色阈值固定为：

```text
remaining >= 60
  -> green

20 <= remaining < 60
  -> yellow

remaining < 20
  -> red
```

说明：初稿中 `45%` 已明确使用黄色，因此这里按 20–60 黄色锁定，消除“40–60 无颜色规则”的歧义。

边界：

```text
60.0 -> green
20.0 -> yellow
19.999... -> red
```

禁止做渐变色。

## 10.6 底部重置时间

ready 且有 reset：

```text
下次重置 · MM/DD HH:mm
```

由浏览器本地 timezone 格式化。

例：

```text
下次重置 · 08/12 12:23
```

reset null：

```text
下次重置 · —
```

---

# 11. Card 状态

## loading

使用现有 KPI Skeleton 规则，不显示假百分比。

## ready

正常显示：

```text
plan
remaining %
bar
reset
popover
```

## auth_required / unavailable 且无 last-good

显示：

```text
剩余配额

—

暂时无法获取配额
```

右上角 plan 不显示伪值。

不得把失败状态显示成：

```text
0%
```

因为 `0%` 是真实的“额度耗尽”状态。

---

# 12. KPI Grid 布局

Quota 固定放最后：

```text
Token -> Cache -> Sessions -> Cost -> Quota
```

无 model filter 时桌面逻辑：

```text
Token  = 剩余宽度
Cache  = 236px
Session= 236px
Cost   = 236px
Quota  = 236px
```

grid 目标：

```text
minmax(0, 1fr) + repeat(4, 236px)
```

有 model filter 时现有 Session 卡不显示：

```text
Token -> Cache -> Cost -> Quota
```

此时必须改为：

```text
minmax(0, 1fr) + repeat(3, 236px)
```

不得保留一列空的 236px Session slot。

Quota 加入后，宽屏呈现结果就是：

```text
Token 卡比 v0.2.0 变窄
其他固定卡尺寸不变
Quota 与其他固定卡同尺寸
```

响应式原则：

- 保留当前 MU 的移动/窄屏 grid 行为；
- Quota 使用和其他固定 KPI 相同断点规则；
- 不为 Quota 单独建立 width；
- 如果当前 `1280` 一行布局因为第 5 张卡产生内容溢出，只允许把“一行五卡”的断点上移到能完整容纳内容的最小宽度，不能缩小 236px 固定卡，也不能压缩现有字体；
- 浏览器 Gate 必须覆盖 1440px、1280px、767px，最终不得横向溢出。

---

# 13. 双 Subagent 并行施工

为了避免互相覆盖文件，先冻结 API Contract，再并行。

## Phase 0 — Luna 主控：冻结 Contract

只做以下确认，不写业务代码：

```text
GET /api/codex/quota
CodexQuotaResponse JSON shape
status enum
Weekly 字段
plan_type raw
email
reset credits
5min refresh
颜色 slot/阈值
```

完成后立即启动两个 Subagent。

---

## Track A — Subagent A：Rust 后端

文件所有权：

```text
src/codex/quota/**          # 新增
src/codex/mod.rs
src/api.rs
src/main.rs
Cargo.toml                  # 仅新增 base64 依赖；版本号由最终集成阶段改
Cargo.lock                  # 仅依赖变化
后端 quota 相关 tests
```

Track A 不修改：

```text
frontend/**
src/scanner/**
src/storage/**
src/usage/**
src/cost/**
现有 migration
.github/**
```

施工顺序：

### A1. auth.rs

完成：

- auth.json Value-preserving read/write；
- access/refresh/id/account token extraction；
- JWT `exp`；
- email；
- ChatGPT account id fallback；
- 5min needs-refresh；
- 8-day fallback；
- live reload。

### A2. client.rs

完成：

- `/wham/usage`
- OAuth refresh
- timeout
- headers
- status mapping
- one auth retry 所需 client seam

### A3. mapper.rs

完成：

- duration 分类
- Weekly-only output
- header percent fallback
- remaining
- reset_at/reset_after
- plan_type raw
- reset credit count

### A4. mod.rs / service

完成：

- DTO/domain snapshot
- last-good
- loading/auth_required/unavailable
- single-flight
- immediate + 5min background timer

### A5. API / main 接入

完成：

- AppContext
- `/api/codex/quota`
- background task lifecycle
- 不阻塞 startup
- 不接 Scanner refresh

### A6. Track A Gate

只跑第 15 节 Backend 必要测试。

通过后冻结 Track A 文件，等待合并。

---

## Track B — Subagent B：Frontend

文件所有权：

```text
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts
frontend/src/data/usagiClient.test.ts
frontend/src/dashboard/useCodexQuotaController.ts      # 新增
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/MetricGrid.test.tsx
frontend/src/dashboard/DashboardPage.tsx
frontend/src/dashboard/DashboardPage.test.tsx         # 仅确有需要
frontend/src/dashboard/format.ts                      # 仅 plan/reset formatter
frontend/tests/browser/dashboard.spec.ts              # 只加一个 Quota browser case
```

只读复用，不修改：

```text
frontend/src/dashboard/charts/chartPalette.ts
frontend/src/ui/beui/tilt-card.tsx
frontend/src/ui/beui/number-ticker.tsx
frontend/src/ui/beui/popover.tsx
frontend/src/ui/lib/**
frontend/src/theme/beui.css
```

Track B 不修改：

```text
src/**
Cargo.toml
Cargo.lock
.github/**
```

施工顺序：

### B1. types + client parser

按 Phase 0 Contract 编写，不等待真实后端。

### B2. controller

用 fake client 完成：

- initial read
- loading 1s retry
- 5min local read
- cleanup
- 不响应 range/filter/revision

### B3. CodexQuotaCard

直接在 `MetricGrid.tsx` 内实现并复用现有 KPI helpers。

完成：

- plan trigger
- hover Popover
- percentage
- public palette
- threshold
- bar
- reset text
- loading/unavailable

### B4. Grid

完成：

- Quota 最后
- 236px
- Token 变窄
- model filter 时不留空 fixed slot
- responsive

### B5. Dashboard 接入

`DashboardPage` 创建 Quota controller，将 snapshot 传给 MetricGrid。

禁止接：

```text
view.request_refresh
revisionFeed
range
filters
```

### B6. Track B Gate

只跑第 15 节 Frontend 必要测试。

通过后冻结 Track B 文件，等待合并。

---

# 14. 合并与版本发布步骤

## S1 — 合并 Track A / B

Luna 主控合并时只解决接口接线问题。

禁止借合并之机：

- 重排整个 Dashboard；
- 重命名现有 API；
- 清理无关 warning；
- 改 Scanner；
- 改既有 Card 视觉。

## S2 — 真实 Codex 联调

使用当前已登录 Codex 账户。

确认：

```text
email
plan_type / plan display
Weekly used/remaining
reset time
reset credit count
```

与 `/wham/usage` 实际响应一致。

不得在终端记录或文档保存 Authorization token。

## S3 — 版本号更新到 0.2.1

Track A/B 和联调 Gate 全通过后再改。

必须更新：

```text
Cargo.toml                     0.2.0 -> 0.2.1
Cargo.lock                     root package version 同步
frontend/package.json          0.2.0 -> 0.2.1
frontend/package-lock.json     同步
```

Frontend 建议：

```bash
cd frontend
npm version 0.2.1 --no-git-tag-version
```

Rust 修改 `Cargo.toml` 后刷新 lock，再验证 `--locked`。

禁止全仓库批量替换 `0.2.0`：

历史实施文档、测试文档文件名里的 `v0.2.0` 保留。

## S4 — Release 前回归

执行第 15.4 Gate。

## S5 — GitHub CI

提交/推送后，必须等待当前 `.github/workflows/ci.yml` 全部通过。

禁止为了本功能修改 CI workflow。

## S6 — 发布 stable 0.2.1

只有：

```text
工作区 clean
本地 Gate PASS
GitHub CI PASS
```

才创建：

```text
v0.2.1
```

push tag 后使用仓库现有 `release.yml`。

Release workflow 必须完整通过 Windows x64 + macOS arm64 构建/打包/smoke。

本轮不要求新增 RC 流程；若 stable tag 前 CI 尚未通过，禁止提前打 tag。

---

# 15. 必要测试与验收

原则：

- 只验证本功能会真实出错的关键边界；
- 不为 DTO 每个字段机械拆一个测试；
- 不复制既有 BeUI primitive 自身测试；
- 不重新测试 Scanner/Token/Cost 内部算法；
- 最终再跑仓库现有回归。

---

## Gate A — Backend Quota

### T-Q-001 Weekly mapper

一条测试覆盖：

- primary=18000；
- secondary=604800；
- weekly used=55；
- remaining=45；
- reset_at -> ms；
- `plan_type=prolite` 原样保留；
- reset credits=2。

PASS：

```text
weekly.limit_window_seconds == 604800
weekly.used_percent == 55
weekly.remaining_percent == 45
reset_credits_available == 2
plan_type == "prolite"
```

### T-Q-002 Weekly-only primary

fixture：

```text
primary_window.limit_window_seconds = 604800
secondary_window = null
reset_at absent
reset_after_seconds present
```

PASS：

- primary 被识别为 Weekly；
- 不误识别 Session；
- reset fallback 正确。

### T-Q-003 OAuth refresh/retry

用 fake HTTP + temp auth.json，一组测试覆盖：

- access token 临近 exp；
- refresh 前重新读 live auth；
- live auth 没新 token 时执行 OAuth refresh；
- 新 access/refresh/id token 写回；
- 未知 JSON 字段仍存在；
- usage 401/403 最多 refresh+retry 一次；
- token 内容不进入错误文本。

### T-Q-004 service cache / timer / isolation

用 paused time / fake provider 覆盖：

- startup background 立即一次；
- 下一次在 300s 后；
- 同时只一个 fetch；
- 成功后下一轮失败保留 last-good；
- 调 Scanner `/api/refresh` 不增加 Quota provider 调用次数；
- Quota 刷新不改变 scanner revision/status。

Gate A 通过条件：

```text
T-Q-001 ~ T-Q-004 全 PASS
cargo fmt --check PASS
相关 Rust tests PASS
```

---

## Gate B — Frontend Quota

### T-Q-005 client contract + formatter

一组测试覆盖：

- `/api/codex/quota` ready payload 解析；
- `prolite -> Pro 5x`
- `pro -> Pro 20x`
- `plus -> Plus`
- invalid percent / wrong Weekly duration 被拒绝。

### T-Q-006 Card ready / Popover / colors

fixture：

```text
remaining = 45
email = hoge@example.com
plan_type = prolite
reset credits = 2
reset time known
```

PASS：

```text
标题 = 剩余配额
主值 = 45%
右上角 = Pro 5x
Popover = hoge@example.com + 重置卡：2 次
reset = MM/DD HH:mm
```

并验证 threshold helper：

```text
60 -> chartSeriesColor(8)
45 -> chartSeriesColor(5)
20 -> chartSeriesColor(5)
19 -> chartSeriesColor(9)
```

used 部分：

```text
chartMuted
```

### T-Q-007 loading / unavailable

PASS：

- loading 使用 Skeleton；
- unavailable/auth_required 无数据时显示 `—`；
- 不出现假 `0%`。

### T-Q-008 Browser layout

只增加一个 browser case，覆盖：

- 1440：Quota 在最后；Quota 与 Cache/Session/Cost 固定卡同宽；Token 变窄；
- plan hover 打开 Popover；
- 1280：无横向溢出；
- 767：无横向溢出。

Gate B 通过条件：

```text
T-Q-005 ~ T-Q-008 全 PASS
npm test PASS
npm run check PASS
npm run build PASS
```

---

## Gate C — 真实账户联调

只做一次真实账户验收。

前置：

```text
Codex 已通过 ChatGPT OAuth 登录
auth.json 可用
```

核对：

1. Card 能进入 ready。
2. email 与当前 Codex 登录邮箱一致。
3. 右上角账户等级映射正确。
4. Weekly remaining = `100 - /wham/usage weekly used_percent`。
5. reset 时间与服务端 Weekly reset 一致。
6. `重置卡：N 次` 与 `rate_limit_reset_credits.available_count` 一致。
7. 点击 MU 右上角“同步数据”后：
   - Quota 不立即刷新；
   - Card 不进入 loading；
   - Scanner 原行为正常。

不要求人为等待 access token 真实过期；OAuth expiry/refresh 已由 T-Q-003 覆盖。

---

## Gate D — 0.2.1 Release 回归

只跑仓库现有发布必要项：

```bash
cargo fmt --check
cargo check --locked
cargo test --locked

cd frontend
npm test
npm run check
npm run build
```

然后：

```text
GitHub CI 全 PASS
```

创建并 push：

```text
v0.2.1
```

最终：

```text
Release workflow Windows x64 PASS
Release workflow macOS arm64 PASS
Release assets 成功生成
GitHub stable release = v0.2.1
```

---

# 16. Luna 施工 + Gate 总图

```text
                     ┌─────────────────────┐
                     │ Phase 0 冻结 Contract│
                     └──────────┬──────────┘
                                │
                 ┌──────────────┴──────────────┐
                 │                             │
        ┌────────▼────────┐           ┌────────▼────────┐
        │ Subagent A      │           │ Subagent B      │
        │ Rust Backend    │           │ Frontend        │
        │ Auth/Client     │           │ DTO/Client      │
        │ Mapper/Service  │           │ Controller      │
        │ API/Main        │           │ Card/Grid       │
        └────────┬────────┘           └────────┬────────┘
                 │                             │
             Gate A                        Gate B
                 │                             │
                 └──────────────┬──────────────┘
                                │
                       ┌────────▼────────┐
                       │ Luna 主控合并   │
                       │ 只做接口接线    │
                       └────────┬────────┘
                                │
                             Gate C
                         真实 Codex 联调
                                │
                       ┌────────▼────────┐
                       │ 版本升到 0.2.1  │
                       └────────┬────────┘
                                │
                             Gate D
                         发布前必要回归
                                │
                         GitHub CI PASS
                                │
                         tag v0.2.1
                                │
                       Release workflow
                                │
                  Windows PASS + macOS PASS
                                │
                       Stable 0.2.1 发布
```

---

# 17. 完成定义

只有同时满足以下条件，本轮才算完成：

```text
[ ] CodexQuotaCard 已加入 KPI 最后一位
[ ] Weekly 使用服务端真实 quota，不做本地估算
[ ] 后端按 duration 正确分类
[ ] 前端只显示 Weekly
[ ] email 正确
[ ] plan_type 后端原样保留
[ ] plan 前端参考 OpenUsage 映射
[ ] reset credits 显示“X 次”
[ ] 60+ 绿色 / 20~<60 黄色 / <20 红色
[ ] 颜色全部来自 MU 公共 chart palette
[ ] BeUI TiltCard / NumberTicker / Popover 直接复用
[ ] Quota 5 分钟独立刷新
[ ] OAuth refresh 可安全更新 auth.json
[ ] 现有同步按钮与 Quota 无关联
[ ] 无 DB migration
[ ] 双 Subagent 文件范围无交叉施工
[ ] Gate A / B / C / D 全 PASS
[ ] Cargo + frontend 版本均为 0.2.1
[ ] GitHub CI PASS
[ ] v0.2.1 Release workflow PASS
[ ] Windows/macOS release assets 生成
[ ] Stable 0.2.1 发布
```
