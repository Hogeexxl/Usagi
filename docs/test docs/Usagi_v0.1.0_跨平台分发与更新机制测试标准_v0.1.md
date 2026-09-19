# Usagi v0.1.0 跨平台分发与更新机制测试标准 v0.1

> 版本：v0.1  
> 日期：2026-08-14  
> 对应实施方案：`Usagi_v0.1.0_跨平台分发与更新机制实施方案_v0.1.md`  
> 代码基线：实施方案记录的当前 Usagi v0.1.0 功能基线  
> 适用范围：Windows/macOS 跨平台运行、正式安装包、嵌入式前端、启动器、GitHub CI/Release、版本检查与更新提示  
>
> **本文是 Usagi v0.1.0“跨平台分发与更新机制”范围内的唯一正式测试执行标准。**  
> 本轮功能语义以对应实施方案为准；实施方案 S0～S13 中的 Gate 继续作为施工阶段门禁，但不得替代本文正式测试条目。最终是否可发布 v0.1.0，以本文完成门为准。

> **范围决策（2026-08-14）**：v0.1.0 正式测试与发布只覆盖两个安装包：Windows 10/11 x64 安装包和 macOS Apple Silicon arm64 DMG，另附 `SHA256SUMS.txt`。macOS Intel/x86_64 不属于本版本正式支持范围，不纳入 runner、构建、smoke、Release asset、Gate 或 DoD；这是范围决策，不表示 Intel 测试失败。

---

# 1. 测试边界与条目设立原则

## 1.1 本文只验证本轮新增风险

本轮不重新定义或复制已经稳定的：

```text
Token 解析/聚合口径
Session / Main / Subagent 业务口径
费用计算口径
reasoning effort
原有 Dashboard KPI/Session 业务行为
原有 scanner 增量/rebuild/carry 业务语义
```

这些能力继续由仓库现有测试作为回归基线。

本文新增测试只覆盖：

```text
Cargo 非 vendor 构建
平台路径
Windows 文件身份
Windows scanner 跨平台闭环
嵌入式前端
启动器 / 重复启动 / 端口冲突 / 浏览器
UpdateService
Update API
前端检查更新按钮
公开仓库发布准备
GitHub CI
GitHub Release
Windows 安装包
macOS arm64 分发包
真实已发布 Release 的更新检查
本轮改动后的全量回归
```

---

## 1.2 条目精简原则

一个测试条目表示一个**可验证的行为或故障矩阵**，不等于一个 Rust `#[test]`、一个 Vitest case 或一个 GitHub Actions step。

优先按独立故障边界合并：

```text
同一状态机的多个输入 → 一个 table-driven / 参数化矩阵
同一平台契约的多个典型路径 → 一个平台矩阵
同一安装包从安装到启动 → 一个真实 artifact smoke
```

明确不要求：

```text
为 Windows 每一种盘符建立测试号
为每一种 GitHub HTTP 错误建立独立测试号
为每一种 semver 组合建立独立测试号
为每一个静态资源文件建立测试号
为每一种 installer UI 页面建立测试号
为每一种 CPU / Windows 小版本建立测试号
为了测试极低概率故障新增与生产无关的 public seam
用“编译成功”冒充“安装包可运行”
用 jsdom 冒充真实二进制静态资源/安装包验收
用真实等待 4 小时测试定时器
```

---

## 1.3 测试真实性原则

1. Windows 文件身份必须用真实临时文件/目录验证，不允许 mock 一个固定 ID 冒充生产语义。
2. Scanner 集成测试继续使用真实临时 `CODEX_HOME`、真实 rollout 文件与真实 SQLite/Ledger。
3. UpdateService 的 GitHub 网络层允许 mock/fake adapter，以保证错误矩阵确定性；但最终发布后必须另有一条真实 GitHub Release E2E。
4. 正式 artifact 测试必须使用 GitHub Release workflow 实际产生的二进制/安装包，不允许用 `cargo run` 替代。
5. 不允许通过：
   - `#[ignore]`；
   - `#[cfg(unix)]` 包住本应跨平台的整套逻辑；
   - 放宽断言；
   - Windows 固定 `(0,0)` identity；
   - 硬编码 API 返回；
   - 跳过安装流程；
   来制造 PASS。
6. 如果 P0/P1 失败，应优先修生产实现；不得为了关闭 Gate 修改测试语义。

---

# 2. 优先级与完成门规则

## 2.1 P0

发布正确性硬门槛。

覆盖：

```text
Windows/macOS 可运行
路径与数据兼容
文件身份正确
正式 binary 自包含
启动生命周期
更新服务不阻塞主程序
更新 UI 状态正确
CI/Release 真实产物
安装包真实启动
版本身份一致
最终回归
```

**任一 P0 FAIL，禁止发布 v0.1.0。**

---

## 2.2 P1

重要发布质量与用户交付边界。

例如：

```text
README / 公共仓库清理
未签名 macOS 使用说明
发布资产完整性/用户可理解性
```

**所有 P1 必须 PASS 后才能关闭本轮正式发布 Gate。**

---

## 2.3 P2

只用于成本较高、非日常开发 Gate 的资源/压力复核。

本轮不新增大规模新的 P2 矩阵，只要求：

- 对本轮实际修改到的 scanner/file identity/RSS 平台适配路径，执行已有相关资源测试；
- 复用原有预算，不为分发版本重新发明性能阈值。

v0.1.0 是正式公开发布，因此本文登记的 P2 在 **FINAL Gate 必须执行**。

---

# 3. Gate 与送测时机

| Gate | 对应实施阶段 | 正式测试 | 目的 |
|---|---|---|---|
| Gate A — Build Base | S1 | T-DIST-001 | 先证明公开仓库可从 lockfile 正常构建，不依赖缺失的 vendor |
| Gate B — Platform Core | S2～S4 | T-DIST-002～004 | 证明 macOS 兼容、Windows 路径和物理文件身份是真实实现 |
| Gate C — Runtime | S5～S6 | T-DIST-005～006 | 证明正式 binary 自包含前端，启动/重复启动/端口行为正确 |
| Gate D — Update | S7～S9 | T-DIST-007～009 | 冻结 UpdateService、API、前端按钮完整状态机 |
| Gate E — Public/CI | S10～S11 | T-DIST-010～011 | 证明公开仓库可交付，Windows/macOS 真环境 CI 全绿 |
| Gate F — Packaging | S12 | T-DIST-012～014 | 证明版本一致、Windows/macOS 正式安装包可运行 |
| Gate G — Published Release | S13 | T-DIST-015 | 用真实公开 GitHub Release 验证更新检查闭环 |
| FINAL | 全部完成 | T-DIST-001～016 + T-DIST-017 | 最终公开发布门 |

施工阶段允许运行更细的定向 unit test 辅助开发；正式 Gate 只在对应施工波合并后执行。

---

# 4. 正式测试条目

| ID | 依赖分类 | 优先级 | 计划执行点 | 测试条目 | 当前状态 | 测试状态 | 计划测试/证据落点 | 缺口 / 说明 |
|---|---|---:|---|---|---|---|---|---|
| **T-DIST-001** | 独立闭环 | P0 | S1 / Gate A | **非 vendor + locked 构建矩阵**：仓库不再强制 `crates.io -> vendor`；无 `vendor/` 目录的 fresh checkout 可用 `Cargo.lock` 完成 `cargo check/test --locked`；前端 `npm ci` 可正常安装；运行时版本只取 `CARGO_PKG_VERSION`。 | ⏳ 待实现 | 未进行 | `.cargo/config.toml` 静态 guard + macOS/CI fresh checkout build | 不要求离线构建；不得重新提交 vendor 规避。 |
| **T-DIST-002** | 独立闭环 + 兼容回归 | P0 | S2 / Gate B | **平台路径矩阵**：显式 `db_path/codex_home` 优先；`CODEX_HOME` 次之；默认值最后。macOS 默认 DB 必须仍为 `~/Library/Application Support/Usagi/mu.sqlite3`，默认 Codex 为 `~/.codex`；Windows 默认 Codex 为用户 Home 下 `.codex`，DB 为 Windows Local AppData/Usagi。包含空/缺环境变量与 Unicode 用户目录。 | ⏳ 待实现 | 未进行 | `src/platform/paths` table-driven unit tests + platform integration | macOS 默认 DB 地址变化直接 FAIL，防止旧用户出现第二个空数据库。 |
| **T-DIST-003** | 独立闭环 + Scanner 前置联动 | P0 | S3 / Gate B | **跨平台物理文件 identity 矩阵**：同一文件重复读取 identity 稳定；两个不同文件 identity 不同；rename 后同一物理文件保持可识别语义；same-path replacement 得到不同 identity/generation；读前/读后/path 最终 identity 不一致时 ChunkReader 拒绝继续提交。Windows identity 不得为统一 `(0,0)`。 | ⏳ 待实现 | 未进行 | `src/platform/file_identity` + `src/scanner/chunk_reader` 私有/集成测试 | 必须使用真实临时文件；禁止只测试 struct 比较。 |
| **T-DIST-004** | 前置联动：现有 scanner 测试 | P0 | S3～S4 / Gate B | **Windows scanner 真实闭环与测试平台化**：真实 Windows runner 能编译并运行 discovery/chunk reader/scanner 关键测试；两个 rollout 不被错误去重；同一路径 replacement/truncate 能触发既有安全处理；通用测试不因 `std::os::unix`、`ps` 等 Unix-only 依赖失败。真正 OS 特有测试可 target-specific，但不得把整个 scanner 套件从 Windows 跳过。 | ⏳ 待实现 | 未进行 | Windows `cargo test --locked` + scanner integration；平台 RSS adapter 相关测试 | “Windows cargo check 通过”不足以 PASS。 |
| **T-DIST-005** | 独立闭环 + 前端联动 | P0 | S5 / Gate C | **正式 binary 自包含前端**：production/release binary 被复制到一个没有源码仓库、没有 `frontend/dist` 的临时目录后启动；`/api/health`、`/`、JS/CSS/font/MIME、SPA fallback 均正常，Dashboard 可加载；不存在运行时回退去读仓库 `frontend/dist`。开发模式仍可使用现有 filesystem/dev 路径。 | ⏳ 待实现 | 未进行 | release build smoke + real HTTP/browser smoke | 必须测正式 embedded 路径，不能只测 Vite dev server。 |
| **T-DIST-006** | 独立闭环 | P0 | S6 / Gate C | **启动器生命周期矩阵**：第一次启动成功 bind `127.0.0.1:3210` 后调用默认浏览器打开 Dashboard；第二次启动探测到具有 Usagi health marker 的已有实例时不得创建第二 scanner/DB worker，只重新打开页面并退出；3210 被非 MU 程序占用时返回明确用户可理解错误，不 panic、不 kill 对方；服务仍不得绑定 `0.0.0.0`。 | ⏳ 待实现 | 未进行 | launcher unit/integration + real process smoke；最终 artifact GUI smoke | OS 默认浏览器“窗口确实弹出”允许在最终 Windows/macOS GUI smoke 人工确认一次；其余逻辑必须自动化。 |
| **T-DIST-007** | 独立闭环 + 主程序联动 | P0 | S7 / Gate D | **UpdateService 状态机/调度/隔离矩阵**：核心服务已可用后立即异步检查；使用 paused/fake time 验证 4h 周期（不得真实等待 4h）；newer/equal/older/invalid tag/timeout/DNS/HTTP error/invalid JSON 状态正确；自动与手动并发检查 single-flight；失败不得抹掉已知新版；GitHub 慢/挂/失败不得阻塞 Usagi 启动、Ledger、Scanner、API、手动同步。 | ⏳ 待实现 | 未进行 | `src/update` table-driven tests + startup integration | 外部 GitHub 在此条允许 mock adapter；禁止把网络成功作为测试前置。 |
| **T-DIST-008** | 前置联动：S05 HTTP 安全 | P0 | S8 / Gate D | **Update API 契约矩阵**：`GET /api/update/status` 返回当前版本、latest/update_available/release_url/last_checked/check 状态的固定 DTO；`POST /api/update/check` 只触发/复用单次检查；`POST /api/update/open-release` 仅在已有合法 Release URL 时打开对应页面。继续受既有 Host/Origin/Sec-Fetch/loopback 安全边界保护，响应不泄漏 GitHub 原始错误/Token/路径。 | ⏳ 待实现 | 未进行 | real Axum router integration + DTO tests | API 字段在 S8 Gate 冻结；前端不得直接请求 `api.github.com`。 |
| **T-DIST-009** | 前置联动：S8 | P0 | S9 / Gate D | **前端更新按钮完整状态机**：默认 `检查更新`；主动点击仅本按钮进入 `检查中…`，其它 Dashboard 操作不被禁用；latest 时提示“当前已是最新版本 vX.Y.Z”并恢复按钮；newer 时变 `版本升级`；自动后台发现新版时不弹强制窗口，只把按钮变为 `版本升级`；主动失败给用户提示，自动失败静默；点击 `版本升级` 调用后端打开准确 Release。页面以本地 status 初始读取 + 60s localhost 轮询感知后端 4h 检查结果，timer/unmount 不泄漏，不产生外网前端请求。 | ⏳ 待实现 | 未进行 | `useUpdateController` + `UpdateButton` tests，fake timers；必要 browser integration | 60s 前端轮询只是读取 localhost 状态，不等于每 60s 请求 GitHub。 |
| **T-DIST-010** | 独立闭环 | P1 | S10 / Gate E | **公开仓库交付边界**：README 只按当前事实描述 Windows/macOS 安装、源码构建、未签名 macOS 首次允许方式、更新机制；已确认的 LICENSE 存在；tracked source 不包含真实 DB/rollout、明显 secret、用户私人绝对路径或 `.cargo/config.toml.saved`；README 不要求最终用户安装 Rust/Cargo/Node/SQLite。 | ⏳ 待实现 | 未进行 | repo static guard + README review | 不把通用示例路径误判为 secret；许可证内容必须来自用户确认。 |
| **T-DIST-011** | 前置联动：全部实现 | P0 | S11 / Gate E | **GitHub CI 真环境矩阵**：frontend `npm ci/test/check/build`；Rust 在 Windows x64 与 macOS arm64 对应环境执行 `cargo check/test --locked`；embedded release smoke 在无 `frontend/dist` 工作目录执行；所有 job 对同一 commit 全绿。Windows job 必须实际执行 T-DIST-003/004 相关测试，不能只 cross-compile。 | ⏳ 待实现 | 未进行 | `.github/workflows/ci.yml` run evidence | runner 名称可随 GitHub 官方支持调整，但“目标架构真实执行证据”不可降级。 |
| **T-DIST-012** | 独立闭环 | P0 | S12 / Gate F | **版本与 Release 资产一致性**：Release workflow 仅正式 `vX.Y.Z` tag 触发；tag `v0.1.0`、`Cargo.toml 0.1.0`、binary reported version、GitHub Release tag/name、两个安装包文件名中的版本一致；不一致时 workflow 必须在上传正式 Release 前失败。生成 `SHA256SUMS`，其中每个正式 asset 均有对应校验值。 | ⏳ 待实现 | 未进行 | release workflow dry-run + artifact metadata assertions | `frontend/package.json` 不参与产品版本判定。 |
| **T-DIST-013** | 前置联动：Windows 全链路 | P0 | S12 / Gate F | **Windows x64 正式安装包 clean-runtime smoke**：只使用 Release workflow 产出的 installer；在与仓库无关的临时用户环境安装，运行目录无源码/`frontend/dist`；用受限 PATH 启动并验证 health + Dashboard；不得调用 Rust/Cargo/Node/npm/SQLite CLI/Visual Studio 工具。若采用静态 CRT，则检查 PE 依赖中不存在未计划的 VC runtime 前置依赖；卸载/覆盖行为至少不破坏用户数据目录。 | ⏳ 待实现 | 未进行 | Windows release artifact install/launch smoke + dependency inspection | GitHub runner 本身装有开发工具不等于测试失效；关键是运行时不解析/调用它们，并检查动态依赖。 |
| **T-DIST-014** | 前置联动：macOS 全链路 | P0 | S12 / Gate F | **macOS arm64 正式分发包 smoke**：Release workflow 产物架构正确；从与仓库无关的位置展开/安装并启动，health + embedded Dashboard 可用，默认 DB/Codex 路径保持平台约定；不依赖源码/Node/Rust。v0.1.0 明确允许未签名，因此不以“Gatekeeper 无警告”为 PASS 条件，但 README 必须给出首次允许运行说明。 | ⏳ 待实现 | 未进行 | macOS arm64 GitHub artifact smoke + `file`/架构检查 | 不要求 Developer ID、notarization 或代码签名。 |
| **T-DIST-015** | 最终外部联动：GitHub Public Release | P0 | S13 / Gate G | **真实公开 Release 更新 E2E**：正式 `v0.1.0` 发布后，真实 Usagi v0.1.0 访问公开仓库 latest release，识别“当前已是最新”；另以不污染正式 tag 的内部测试构建（如 current=`0.0.9`）访问同一个 public latest release，识别 `v0.1.0` 为新版并得到正确 Release URL；不需要 GitHub Token。 | ⏳ 待实现 | 未进行 | real GitHub public API + released repository | 这是唯一必须访问真实 GitHub 网络的更新测试；失败需区分代码错误与 GitHub 暂时不可用。 |
| **T-DIST-016** | 最终回归 | P0 | FINAL | **本轮改动后的完整回归**：macOS 与 Windows 对各自可执行范围运行 `cargo fmt --check`、`cargo check --locked`、无过滤 `cargo test --locked`；前端 `npm run test`、`npm run check`、`npm run build`；原有 Dashboard/Session/refresh/SSE/Scanner 核心链不得因平台/launcher/update 改造回退。正式 Gate 不允许用仅定向测试代替全量回归。 | ⏳ 待实现 | 未进行 | CI final matrix + local macOS evidence | 已有业务测试复用原测试，不在本文复制新的 T-DIST 条目。 |
| **T-DIST-017** | 最终资源复核 | P2 | FINAL | **受影响平台资源测试**：执行本轮因 Windows 适配实际修改到的既有 scanner/chunk-reader/RSS 资源 P2；Windows 使用等价平台 RSS sampler，不以 `ps` 缺失跳过；复用既有批大小/内存/时间预算，不因本轮发布工程另造新性能指标。 | ⏳ 待实现 | 未进行 | 既有 P2 测试 + Windows/macOS 平台 adapter evidence | 只复核“本轮修改路径触及的 P2”，不机械重跑与发布工程无关的所有历史压力矩阵。 |

---

# 5. 各 Gate 的最低执行集

## 5.1 Gate A — S1 Build Base

必须：

```text
T-DIST-001
cargo check --locked
cargo test --locked
frontend: npm ci
```

Gate A 通过前，不进入 GitHub workflow/installer 施工。

---

## 5.2 Gate B — S2～S4 Platform Core

必须：

```text
T-DIST-002
T-DIST-003
T-DIST-004
```

判定重点：

```text
macOS 原默认 DB path 不变
Windows file identity 非占位
Windows scanner 测试真实执行
```

**T-DIST-003/004 未 PASS，禁止声称 Windows 已支持。**

---

## 5.3 Gate C — S5～S6 Runtime

必须：

```text
T-DIST-005
T-DIST-006
```

正式 binary 必须在离开仓库目录后仍可运行。

---

## 5.4 Gate D — S7～S9 Update

必须：

```text
T-DIST-007
T-DIST-008
T-DIST-009
```

这三条一起冻结：

```text
UpdateService 状态语义
Update API DTO
前端按钮状态机
```

通过后不得为了前端方便随意修改 API 字段或后台检查语义。

---

## 5.5 Gate E — S10～S11 Public / CI

必须：

```text
T-DIST-010
T-DIST-011
```

其中 T-DIST-010 为 P1，但公开发布前同样必须关闭。

---

## 5.6 Gate F — S12 Packaging

必须：

```text
T-DIST-012
T-DIST-013
T-DIST-014
```

只允许使用 GitHub Release workflow 产出的正式候选 artifact。

建议先用测试 tag / draft Release 做 dry-run，不把失败候选冒充正式 v0.1.0。

---

## 5.7 Gate G — S13 Published Release

正式发布后必须：

```text
T-DIST-015
```

该条通过后才证明“公开 Release + 客户端更新检查”真正闭环。

---

# 6. FINAL Gate

v0.1.0 最终可分发必须满足：

```text
T-DIST-001 ～ T-DIST-016：全部 PASS
T-DIST-017：PASS
P0：0 FAIL
P1：0 FAIL
P2（本文登记项）：0 FAIL
```

并且：

```text
Windows x64 安装包真实安装/启动通过
macOS arm64 正式产物真实启动通过
公开 GitHub Release v0.1.0 可匿名访问
v0.1.0 客户端真实 latest check 判定为最新版
低版本内部测试构建真实判定 v0.1.0 为可升级版本
```

施工阶段 Gate 曾经 PASS **不能替代 FINAL 重跑**。

---

# 7. 工程命令与执行约束

## 7.1 Rust

正式完成门至少执行：

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
```

要求：

```text
无测试名称过滤
无 --skip
不得临时删除/屏蔽失败测试
平台相关 cfg 只能用于真正 OS-specific 行为
```

### Clippy 规则

`cargo clippy --all-targets -- -D warnings` 本轮作为**发布质量检查**执行，但不允许它迫使 Luna 擅自扩大范围清理历史无关代码。

判定：

```text
如果 S0 baseline clippy 已 PASS
→ FINAL 必须继续 PASS

如果 S0 baseline 已存在与本轮无关的 warning
→ 本轮新增/修改文件不得新增 warning
→ 原有未触及 warning 单独记录，不得通过扩大业务范围清理来冒充本轮完成
```

任何本轮修改直接引入的 Clippy warning 均阻塞对应 Gate。

---

## 7.2 Frontend

正式完成门至少执行：

```bash
npm ci
npm run test
npm run check
npm run build
```

涉及：

```text
UpdateButton
useUpdateController
embedded production frontend
```

不得只以 TypeScript 编译通过代替行为测试。

---

## 7.3 GitHub Actions

正式证据必须来自：

```text
同一 commit
Windows x64
macOS arm64 目标
frontend
release artifact smoke
```

如果某个 GitHub hosted runner 名称或架构供应发生变化，可以调整 workflow 实现，但不能降低目标：

> 每一个对外宣称支持的正式架构，都必须有实际对应的 build + runtime smoke 证据。

---

# 8. 测试代码组织标准

## 8.1 Rust

建议：

```text
src/platform/
  paths.rs
  file_identity.rs
  browser.rs
  tests.rs 或相邻私有 tests

src/update/
  ...
  tests.rs

tests/
  distribution_runtime_integration.rs
  update_api_integration.rs
```

原则：

- 单模块私有算法留在模块 unit test；
- 真实进程、真实 SQLite、真实 filesystem、真实 Axum 使用顶层 `tests/`；
- 不把所有 T-DIST 条目塞进旧 Spec04/05/06 文件；
- 已有 scanner 测试只在确实需要平台化时修改，避免重复建立第二套。

---

## 8.2 Frontend

建议：

```text
frontend/src/dashboard/UpdateButton.test.tsx
frontend/src/dashboard/useUpdateController.test.tsx
frontend/src/data/usagiClient.test.ts
frontend/tests/browser/...
```

纯状态机可使用 Vitest/fake timers。

正式 embedded binary / installer 运行不能由 jsdom 替代。

---

## 8.3 Workflow / Artifact

以下证据不需要伪装成 Rust `#[test]`：

```text
GitHub Actions job success
Release workflow dry-run
artifact architecture
installer install/launch
SHA256SUMS
tag/version consistency
public release latest check
```

它们应保留在 CI/Release 执行记录中，并在最终测试执行记录里映射到对应 T-DIST ID。

---

# 9. 真实安装包测试环境

## 9.1 Windows

最低正式支持：

```text
Windows 10/11 x64
```

测试条件：

```text
从 Release workflow artifact 安装
不在源码仓库目录运行
不依赖 frontend/dist
受限 PATH
真实用户目录 / Local AppData
真实 127.0.0.1:3210
真实浏览器/HTTP smoke
```

不要求用户预装：

```text
Rust
Cargo
Node.js
npm
SQLite CLI
Visual Studio
Windows SDK
```

---

## 9.2 macOS

最低正式支持：

```text
Apple Silicon arm64
```

v0.1.0：

```text
不签名
不 notarize
```

因此本标准**不要求**：

```text
首次双击完全无 Gatekeeper 提示
```

只要求：

```text
README 正确说明首次手动允许方式
用户允许后应用可正常运行
两个正式目标 artifact 均可启动
```

---

# 10. 更新机制的测试口径

## 10.1 自动检查

定义：

```text
Usagi 核心服务已启动
↓
UpdateService 后台立即检查一次
↓
不阻塞主程序
↓
之后每 4h 再检查
```

测试必须使用可控时间/paused time。

禁止：

```text
测试真的 sleep 4h
```

---

## 10.2 前端状态感知

前端不直接访问 GitHub。

正式链路：

```text
GitHub
  ↑
Rust UpdateService
  ↓
/api/update/*
  ↓
React UpdateButton
```

前端为感知后台 4h 自动检查结果，可以每 60s 请求一次 localhost update status。

这 60s 轮询：

```text
不访问 GitHub
不触发新的 GitHub 检查
不阻塞其它 Dashboard 请求
```

---

## 10.3 “版本升级”

v0.1.0 的含义固定为：

```text
发现新版
↓
按钮 = 版本升级
↓
用户点击
↓
打开对应 GitHub Release 页面
```

明确不测试、也不实现：

```text
自动下载
自动执行 installer
程序自覆盖
静默重启
强制升级
```

---

# 11. GitHub Release 测试口径

正式 Release 前先验证候选：

```text
Cargo.toml 0.1.0
Git tag v0.1.0
binary 0.1.0
Release v0.1.0
asset name 0.1.0
SHA256SUMS
```

正式 Release asset 至少包含：

```text
Windows x64 安装包
macOS arm64 分发包
SHA256SUMS
```

具体后缀由最终 packager 配置决定；测试关注：

```text
平台
架构
版本
可安装/可启动
```

不把文件后缀本身当作功能语义。

---

# 12. 与实施方案施工顺序的复核

根据本文正式测试条目，实施方案 S0～S13 的主顺序**无需重排**。

建议只把正式送测点明确成：

```text
S0
↓
S1
↓
Gate A：T-DIST-001
↓
┌──────────── S2-S4 Platform ────────────┐
│                                        │
└──────────── S5 Embedded ───────────────┘
↓
Integration
↓
Gate B：T-DIST-002～004
Gate C（S5部分）：T-DIST-005
↓
S6
↓
Gate C 完整：T-DIST-005～006
↓
┌──────────── S7 UpdateService ──────────┐
│                                        │
└──────────── S10 Repo cleanup ──────────┘
↓
S8 API freeze
↓
S9 Frontend        S11 CI
↓
Gate D：T-DIST-007～009
Gate E：T-DIST-010～011
↓
S12 Packaging
↓
Gate F：T-DIST-012～014
↓
S13 Public Release
↓
Gate G：T-DIST-015
↓
FINAL：
T-DIST-001～017
```

并行协调继续按实施方案执行：

1. S1 完成后，S2～S4 与 S5 可并行。
2. S6 继续单人串行，避免 `main.rs` 启动生命周期冲突。
3. S6 后，S7 与 S10 可并行。
4. S8 API 契约冻结后，S9 与 S11 可以并行，但 S11 最终矩阵需等待 S9 合入后再跑完整版本。
5. S12、S13 必须串行。

测试标准没有发现需要反转施工依赖的新增问题。

---

# 13. 施工与测试禁止项

Luna 在本轮不得：

1. 用 Windows `(0,0)` 文件 identity 通过测试；
2. 删除或弱化 ChunkReader 的物理文件变化检测；
3. 将通用 scanner 测试整体 `cfg(unix)`；
4. 只在 Mac cross-compile 后宣称 Windows 支持；
5. 用 `cargo run` 替代 Windows/macOS 安装包 smoke；
6. 让正式 binary 运行时读取源码目录 `frontend/dist`；
7. 让 React 直接请求 GitHub；
8. 把 GitHub Token/PAT 写进客户端；
9. 用真实等待 4h 测定时器；
10. 自动检查失败时让主程序启动失败；
11. 自动检查失败后清空此前已知新版；
12. 把 `版本升级` 做成自动下载安装；
13. 用 GitHub runner 已安装 Rust/Node 为理由省略 runtime independence 验证；
14. 为让 clippy 全绿擅自扩大范围修改历史无关业务代码；
15. 为测试方便修改 Token/Session/cost 口径；
16. 因 macOS 未签名而把签名/notarization 擅自加入 v0.1.0 范围；
17. 在 Gate F 未通过前创建正式 v0.1.0 Release；
18. 以实施方案阶段 Gate 已 PASS 为理由跳过 FINAL 全量执行。

---

# 14. 最终完成定义

只有以下全部成立，才能认定 **Usagi v0.1.0 跨平台分发与更新机制完成**：

```text
[ ] T-DIST-001～016 全部 PASS
[ ] T-DIST-017 PASS
[ ] Windows x64 真实安装包可安装、启动、加载 Dashboard
[ ] Windows 不要求用户安装 Rust/Cargo/Node/npm/SQLite/Visual Studio
[ ] Windows 使用真实文件 identity，不存在正式 (0,0) fallback
[ ] macOS 原有默认数据库路径保持不变
[ ] macOS arm64 正式 artifact 可启动
[ ] 正式 binary 不依赖 frontend/dist
[ ] 首实例正常启动并打开 Dashboard
[ ] 第二实例不启动第二 scanner
[ ] 非 MU 端口冲突不 panic、不误杀
[ ] UpdateService 启动后异步检查
[ ] 自动检查周期为 4h
[ ] GitHub 异常不影响 Usagi 主功能
[ ] 前端常态为“检查更新”
[ ] 主动检查最新版有明确提示
[ ] 发现新版变为“版本升级”
[ ] 自动发现新版不强制弹窗打断
[ ] 主动失败提示、自动失败静默
[ ] “版本升级”打开正确 GitHub Release
[ ] 前端不直接请求 GitHub
[ ] CI 对 Windows/macOS 目标真实执行并全绿
[ ] Cargo/tag/binary/Release/assets 版本一致
[ ] Release 含两个目标平台/架构产物和 SHA256SUMS
[ ] Public GitHub Release 可匿名下载
[ ] 正式 v0.1.0 可真实检查出“当前已是最新”
[ ] 低版本测试构建可真实发现 v0.1.0
[ ] 本轮全量 Rust/Frontend 回归无新增失败
[ ] README/LICENSE/公开仓库边界符合发布要求
```

---

# 15. 当前状态

本文制定时生产实现尚未按该标准完成最终验收，因此：

- T-DIST-001～016：**0 PASS / 0 FAIL / 16 未进行**
- T-DIST-017：**P2，未进行**
- Gate A～G：**均未按本文正式关闭**
- FINAL Gate：**未关闭**

Luna 可以继续按实施方案施工，并在每个施工阶段用本文对应条目关闭正式 Gate。

最终交付时应另生成：

```text
Usagi_v0.1.0_跨平台分发与更新机制测试执行记录_v0.1.md
```

该执行记录只记录：

```text
实际 commit/tag
实际 GitHub Actions run
实际命令
实际 PASS/FAIL
实际 artifact
实际 Windows/macOS smoke
实际 GitHub Release
```

不得把“计划执行”写成“已经通过”。
