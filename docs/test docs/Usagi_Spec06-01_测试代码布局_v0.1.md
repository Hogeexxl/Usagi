# Usagi Spec06-01 测试代码布局 v0.1

> 本文记录 Spec06-01 已执行验收的实际测试落点。测试条目、优先级、Gate 与通过标准只取自 `Usagi_测试标准_Spec01-06_v0.17.md` §8.1；功能语义取自 `docs/Spec_06_01_前端框架与Dashboard界面_v0.2.md`。本文不新增测试需求，也不把 Spec06-02 或最终完整测试的条目提前纳入 S06-01。

## 1. 布局原则

- 纯 formatter、DTO/runtime validator 和 controller/revision 状态矩阵与实现同目录，分别使用 `*.test.ts` / `*.test.tsx`，通过 Vitest + jsdom + React Testing Library 的 public seam 驱动。
- 跨模块但仍只验证 DOM/状态的 Dashboard 测试留在 `frontend/src/dashboard/` 的组件/controller 测试中；不为测试向 view model 暴露额外字段。
- 真实 Vite proxy、真实 Axum、真实 Chromium 布局/zoom/forced-colors/滚动/资源边界放 `frontend/tests/browser/`，由 `tests/spec06_frontend_browser.rs` 建立临时 fixture 并串起开发服务器与 production `frontend/dist` 两轮。
- 所有 HTTP/浏览器 fixture 固定在 loopback；Rust fixture 使用临时 Ledger 与临时 CODEX_HOME，不读取真实 `~/.codex`，不改写用户 `HOME` 或 `CODEX_HOME`。
- jsdom 只能证明 DOM 结构、可访问性属性、状态 reducer/controller 与 fake EventSource/timer 行为；不能证明像素尺寸、列间距、真实网络代理、Axum Host/Origin/Sec-Fetch 防护或浏览器资源生命周期。

## 2. T-S06-001～014 逐项映射

| 条目 | 生产落点 | 单元 / jsdom 落点 | RTL / 真实浏览器 / Axum 落点 | 当前结论 |
| --- | --- | --- | --- | --- |
| T-S06-001 | `frontend/package.json`、`frontend/tsconfig.json`、`frontend/vite.config.ts`、`frontend/src/index.css`、`frontend/src/assets/fonts/JetBrainsMono-Regular.woff2` 与 `OFL.txt`；`frontend/dist` 由 Vite build 生成并由 Axum static fallback 托管。 | `frontend/src/dashboard/DashboardPage.test.tsx` 断言页面没有导航/图表入口；工程边界由 package/config/typecheck/build 共同检查。 | `frontend/tests/browser/dashboard.spec.ts` 真实页面 smoke；`tests/spec06_frontend_browser.rs::spec06_real_axum_browser_gate` 依次运行 Vite dev 与 Axum dist 两轮。Playwright 仅为 devDependency。 | React 19 + strict TS + Vite 6 + Tailwind 4、最小依赖、脚本、字体与 dist 托管均已验证。 |
| T-S06-002 | `frontend/src/data/types.ts` 精确 canonical DTO；`frontend/src/data/usagiClient.ts` 的 `parseRange`、`parseUsage`、`parseStatus`、`parseRevision`、`parseRefresh`、`parseError`、`getJson` 是唯一 HTTP/DTO seam。 | `frontend/src/data/usagiClient.test.ts` 验证完整 canonical DTO、HTTP/status、安全整数、ratio、nullable cache-write/uncached/cost、refresh ack、旧字段-only 拒绝、固定错误且不携带 body 文本。 | 浏览器 proxy test 实际请求 summary/revision/refresh；错误 sentinel 在 Dashboard browser privacy test 中由真实页面消费。 | canonical DTO runtime 校验、相对 URL、固定安全错误与无 `any` 边界均通过。 |
| T-S06-003 | `frontend/src/dashboard/useDashboardController.ts` 的 `Map<RangeKey, Snapshot>`、range generation、AbortController、mount 并行加载、`selectRange`、`retryLoad`。 | `frontend/src/dashboard/useDashboardController.test.tsx` 覆盖 today/yesterday 自有 snapshot、晚响应丢弃、stale summary 收敛、并行依赖与 retry；`DashboardPage.test.tsx` 覆盖 summary/status/revision 独立失败。 | 浏览器 gate 只作真实 mount/请求链 smoke；像素/网络不能由本项 jsdom 结果替代。 | 当前 range snapshot、竞态与重试要求已覆盖。 |
| T-S06-004 | `frontend/src/dashboard/MetricGrid.tsx` 的固定八卡配置与 null/unknown 映射；`MetricCard.tsx` 的 title/aria-label；`format.ts` 的纯 formatter。 | `MetricGrid.test.tsx`、`format.test.ts` 验证 canonical 顺序/字段、zero/null、Some(0)/null、K/M/B、ratio/cost、完整整数 title/accessible name 与输入不变。 | `dashboard.spec.ts` real layout test 验证八张卡片实际渲染；Axum dist 轮验证 production bundle。 | KPI 口径和展示格式已通过，不由前端重算费用或缓存派生值。 |
| T-S06-005 | `frontend/src/index.css` 的 shell gutter、固定 content padding、237×106 card、auto-fit grid 与 1280/768/767 断点。 | jsdom 组件测试仅辅助确认卡片/页面 DOM；不用于像素或滚动结论。 | `frontend/tests/browser/dashboard.spec.ts` 真实 Chromium 断言 1512/1280/1024/768/767/390、84/1344/1312、237×106、31.75px、列数和 body scrollWidth；dev/dist 两轮均执行。 | 真实布局矩阵 PASS；jsdom 不替代浏览器布局引擎。 |
| T-S06-006 | `RangeSelector.tsx` 的 group/aria-pressed、`DashboardPage.tsx` 的 live/error/status/skeleton、`index.css` 的 2px focus-visible 与 reduced-motion。 | `RangeSelector.test.tsx`、`MetricGrid.test.tsx` 验证 ARIA/pressed/skeleton；Dashboard component tests 验证固定错误与状态结构。 | `dashboard.spec.ts` 真实 Chromium 运行键盘 focus、`prefers-reduced-motion`、forced-colors 与 200% page scale；dev/dist 两轮执行。 | DOM 可访问性和浏览器操作边界均 PASS；hover 不是数据入口。 |
| T-S06-007 | `useDashboardController.ts::requestRefresh` 的 status/binding/target/active/queued/requesting guard；`usagiClient.ts::refresh` 的 POST 与 `X-Usagi-Request: 1`。 | `useDashboardController.test.tsx` refreshable-status table、started target、重复点击；`usagiClient.test.ts` header/ack。 | browser Vite→Axum test 发真实 refresh POST；后端 coalescing 复用 S03/S05 已有测试，不在此复制。 | 前端请求边界、header 与单在途 refresh PASS。 |
| T-S06-008 | `useDashboardController.ts` 的 `targetRef`、target kind、`reduceStatus` 与 target URL。 | `useDashboardController.test.tsx` 覆盖 202 Started、200 Coalesced、cached terminal、target terminal→follow-up、same-ID coalesced 和 target state。 | browser API smoke 验证真实 refresh ack 与 status/events 入口；Axum fixture 提供真实 HTTP 链。 | Started/follow-up ID 与 target-only 归约 PASS。 |
| T-S06-009 | `DashboardPage.tsx` 的固定文案映射；controller 的 refresh/status generation、tracking_error、独立 retry；client 的安全 error code。 | `DashboardPage.test.tsx` 403/409/500、tracking status retry、普通依赖失败隔离；`usagiClient.test.ts` 验证 body 不进 Error。 | `dashboard.spec.ts` 真实页面注入含 SQL/Prompt/JSONL sentinel 的 500 body，检查 UI 与 console 不泄漏。 | 错误/竞态/固定文案边界 PASS。 |
| T-S06-010 | controller mount recovery、follow-up/active target 选择、queued/running/terminal reducer 与 follow-up 接续。 | `useDashboardController.test.tsx` 覆盖 queued/active/start_failed reload、Busy queued retry、terminal→follow-up、重复 coalesced。 | Axum fixture 的 `/api/status?target_scan_id=...` 与 events 由 browser smoke 实际访问；不在前端猜测完成。 | 持久化目标恢复与 Busy/start_failed 边界 PASS。 |
| T-S06-011 | controller 的 EventSource、tuple 单调比较、60s poll timer、AbortController 和 effect cleanup。 | `useDashboardController.test.tsx` fake EventSource/fake timers 覆盖单 timer、equal tuple 去重、data/status 分量独立触发与有效事件停 timer。 | `dashboard.spec.ts` 真实 `/api/events` stream smoke；`tests/spec06_frontend_browser.rs` dev/dist 两轮运行。SSE payload 只作 revision hint。 | revision transport、fallback 与清理 PASS。 |
| T-S06-012 | `frontend/vite.config.ts` `/api` target、`changeOrigin`、`proxyReq` Origin rewrite；client 所有 API 均为同源相对路径。 | config/typecheck/build 只验证结构，不能单独作为 proxy 集成证据。 | `dashboard.spec.ts` 通过真实 Vite dev server 请求 summary/events/refresh，并直接向 Axum 发送 wrong Host/Origin/Sec-Fetch 断言 403；production dist 轮同样执行。 | 真实代理链与 S05 安全防护 PASS。 |
| T-S06-013 | controller 的 per-range stable snapshot 与 data-revision 追平；`MetricGrid.tsx` 一次 usage snapshot 映射八卡、120ms identity animation。 | `useDashboardController.test.tsx` stable KPI while target runs、completion refresh、stale response；`DashboardPage.test.tsx` stable snapshot/error/failed load；MetricGrid snapshot identity test。 | browser dev/dist 轮确认实际页面在 API 状态变化下仍保持布局与稳定渲染。 | 一次性替换、旧值保留、合法零值和布局稳定 PASS。 |
| T-S06-014 | 本地 font asset/CSS；client 相对 API/fixed errors；`DashboardPage.tsx` 无 settings/远程连接/导航入口。 | `DashboardPage.test.tsx`、`usagiClient.test.ts` 检查原始 body 不进入 UI/Error；package/source static review 检查无运行依赖外连或持久化 cache。 | `dashboard.spec.ts` route guard 阻断跨源请求，storage/IndexedDB 写入 guard、console/UI sentinel 和直接 Axum 安全拒绝；dev/dist 两轮执行。 | 本地资源、同源、隐私泄漏与浏览器持久化边界 PASS。 |

## 3. jsdom 与真实浏览器边界

- `frontend/src/**/*.test.ts(x)` 的实际通过数以当前 `npm test -- --run` 输出为准（本轮 74/74，10 个文件），证明 DTO、纯函数、controller 状态、React DOM/ARIA、Abort/generation/fake timer seam；它们不宣称真实 CSS 像素布局或 Vite 网络代理通过。
- `frontend/tests/browser/dashboard.spec.ts` 当前包含 9 个真实 Chromium tests，负责布局/列数/scrollWidth、focus/zoom/reduced-motion/forced-colors、同源请求、真实 events/refresh 与 host/origin/sec-fetch 拒绝，以及 Session 分页/重启恢复/压力；Session 专项映射见 Spec06-02 布局文档。
- `tests/spec06_frontend_browser.rs::spec06_real_axum_browser_gate` 使用临时 Ledger、临时 CODEX_HOME 与真实 Axum router，先跑 Vite dev proxy，再跑 Axum 托管的 `frontend/dist`；每轮实际通过数以 Playwright 输出为准，不能沿用历史计数。
- Browser fixture 不读取真实 `~/.codex`，不改写 `HOME`；测试结束清理临时目录和 scanner。任何生产环境已有进程或用户页面不属于本布局的 fixture。

## 4. 延期条目

T-S06-015（P2）是 Dashboard 生命周期/并发资源压力，按测试标准延期到最终完整测试；本布局不把当前 Vitest/browser 数量或工程命令 PASS 解释为该压力条目已完成。T-S06-016～030 属于 Spec06-02，T-FINAL-001～016 属于 S06 总 Gate 关闭后的最终发布门，均不在本文件标 PASS。
