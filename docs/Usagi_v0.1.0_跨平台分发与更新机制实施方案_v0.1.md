# Usagi v0.1.0 跨平台分发与更新机制实施方案 v0.1

> 方案版本：v0.1  
> 日期：2026-08-14  
> 代码基线：用户提供的最新代码包 `Usagi-653321c`  
> 当前应用版本：`0.1.0`（`Cargo.toml`）  
> 目标：把当前已完成的 Usagi 功能基线整理为第一个可公开分发版本，并支持 Windows/macOS、GitHub Release 与版本更新提示。  
> 注意：本文是**实施方案**，不是独立测试标准。每阶段只定义施工 Gate；正式测试条目另行编写。

> **范围决策（2026-08-14）**：v0.1.0 正式分发收敛为两个安装包：Windows 10/11 x64 安装包与 macOS Apple Silicon arm64 DMG，另附 `SHA256SUMS.txt`。macOS Intel/x86_64 不属于本版本正式支持范围，因此不纳入 CI runner、构建、smoke、Release asset、Gate 或 DoD；这是范围决策，不表示 Intel 测试失败。

---

# 0. 执行边界

## 0.1 本版本必须完成的目标

本轮只做“发布工程 / 跨平台运行 / 更新机制”，不重新设计现有 Usage、Session、费用、Scanner 和 Dashboard 业务口径。

必须完成：

1. Usagi 源码进入**一个公开 GitHub 仓库**；源码与 Releases 使用同一仓库。
2. 支持以下正式发布目标：
   - Windows 10/11 x64；
   - macOS Apple Silicon arm64；
3. 最终普通用户运行 Usagi 时，不要求预先安装：
   - Rust；
   - Cargo；
   - Node.js；
   - npm；
   - SQLite；
   - Visual Studio / Windows SDK。
4. React/Vite 构建产物随正式 Rust 二进制发布，正式运行时不得依赖仓库中的 `frontend/dist` 相对路径。
5. 用户启动 Usagi 后：
   - 启动本地 Rust 服务；
   - 继续只监听 `127.0.0.1:3210`；
   - 自动打开默认浏览器进入 Dashboard。
6. 处理重复启动和真实端口冲突，不允许第二次启动直接 panic。
7. 增加后端 UpdateService：
   - 应用核心启动完成后立即异步检查一次；
   - 之后每 4 小时自动检查一次；
   - GitHub 不可访问时不得阻塞或破坏 Usagi 主功能。
8. Dashboard 增加更新按钮：
   - 常态：`检查更新`；
   - 主动检查中：`检查中…`；
   - 已是最新：提示“当前已是最新版本 vX.Y.Z”，按钮恢复 `检查更新`；
   - 有新版本：按钮变为 `版本升级`；
   - 自动检查发现新版：不弹强制窗口，只把按钮改成 `版本升级`；
   - 点击 `版本升级`：打开对应 GitHub Release 页面；
   - 不做应用内下载和自动覆盖安装。
9. 增加 GitHub Actions：
   - CI：在真实 Windows/macOS runner 上构建/测试；
   - Release：Tag 发布时自动构建上述两个正式安装包并上传同一 GitHub 仓库的 Releases。
10. macOS v0.1.0 **不做 Developer ID 签名、不做 notarization**，README 必须明确首次运行可能需要用户手动允许。

---

## 0.2 本版本明确不做

```text
Linux 正式发行
Windows ARM64 正式发行
Tauri / Electron / WebView 重构
系统托盘
Windows Service
macOS LaunchAgent
开机自动启动
静默后台常驻机制重构
应用内自动下载新版
应用内自动替换/覆盖安装包
差分更新
更新强制策略
更新渠道 stable/beta 多通道
macOS 代码签名 / notarization
Windows 代码签名
修改 Token / Session / cost / reasoning / aggregation 业务口径
修改 scanner 定时周期与现有手动刷新协议
改变 127.0.0.1:3210 的本地安全边界
为了 Windows 简单删除现有文件身份/TOCTOU 防护
```

本轮目标是：**保持当前 Rust + Axum + React 浏览器架构，只补齐正式分发所需的外围能力。**

---

## 0.3 发布前仍需用户提供的唯一外部参数

Luna 可以完成全部代码和流水线施工，但以下内容不得自行猜测：

1. GitHub 最终仓库：`<github_owner>/<github_repo>`；
2. 开源许可证选择。

代码中允许在施工阶段使用集中式占位配置，但 **Gate Release 前必须替换为真实仓库坐标**。

许可证属于仓库公开发布决策。Luna 不得擅自替用户选择许可证；仓库转 Public 前必须已有用户确认的 `LICENSE`。

---

# 1. 当前代码事实与必须修复的发布差距

## 1.1 当前版本号已经可以作为 v0.1.0 基线

当前 `Cargo.toml`：

```toml
[package]
name = "usagi"
version = "0.1.0"
edition = "2024"
```

本轮不重新命名版本。

正式版本规则统一为：

```text
Cargo.toml version = 0.1.0
Git tag            = v0.1.0
GitHub Release     = v0.1.0
```

运行时版本号只允许从：

```rust
env!("CARGO_PKG_VERSION")
```

获取。

`frontend/package.json` 中的版本只属于 npm package metadata，不得作为 Usagi 更新判断依据。

---

## 1.2 当前 `.cargo/config.toml` 不能直接公开使用

当前：

```toml
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
```

但代码包没有 `vendor/`。

因此全新 checkout 可能出现：

```text
Cargo 被强制要求只使用 vendor/
→ vendor/ 不存在
→ 无法正常构建依赖
```

本轮处理：

```text
取消 crates.io -> vendor 的强制 replacement
不提交 vendor/
保留 Cargo.lock
CI / Release 使用 cargo --locked
```

`.cargo/config.toml.saved` 属于本地遗留备份配置，不应作为公开源码的一部分。完成配置迁移后清理。

不得通过重新生成整个 `vendor/` 来规避这个问题；本项目没有离线构建要求。

---

## 1.3 当前数据库默认路径只按 macOS 编写

当前 `src/storage/mod.rs`：

```text
DEFAULT_APP_DIR = Library/Application Support/Usagi
home_dir()      = $HOME
```

Windows 不应依赖 `$HOME`，应用数据也不应写到 macOS 路径结构。

必须改成平台标准目录解析。

目标优先级保持：

```text
LedgerOptions 显式 db_path / codex_home
        ↓
环境变量 CODEX_HOME
        ↓
平台用户 Home/.codex
```

数据库默认目录：

```text
macOS:
~/Library/Application Support/Usagi/mu.sqlite3

Windows:
使用 Windows Local AppData 下的 Usagi 应用数据目录
```

实现时使用平台目录库，不再手写 `$HOME` / `%USERPROFILE%` 分支。

重点要求：**必须保持现有 macOS 默认数据库位置不变**，避免现有用户升级 v0.1.0 后突然生成第二个空数据库。

---

## 1.4 Windows 最大的生产代码阻塞不是路径，而是文件身份

当前 `src/scanner/chunk_reader.rs` 顶层直接：

```rust
use std::os::unix::fs::MetadataExt;
```

并使用：

```text
metadata.dev()
metadata.ino()
metadata.mtime()
metadata.mtime_nsec()
```

Windows 无法直接编译这部分。

更重要的是，当前 `src/scanner/discovery.rs` 已存在一个 `#[cfg(not(unix))]` 分支，但它把文件身份写成：

```text
(device_id, inode) = (0, 0)
```

这只能算占位，不是正式 Windows 实现。

因为 Usagi 当前会依赖物理文件身份完成：

```text
Discovery 去重
source_files 身份判断
file_generation
checkpoint / build proof
ChunkReader 读前、读后、path 最终身份确认
文件在扫描期间被替换/切换时的安全拒绝
```

如果所有 Windows rollout 都是 `(0,0)`：

```text
不同文件可能被当成同一个物理文件
→ 去重、增量状态和安全保护全部失去正确语义
```

因此 Windows Track 的第一核心任务是建立**跨平台文件身份层**，绝对禁止通过删除 identity 比较、固定写 0 或放宽断言来让 Windows 编译通过。

---

## 1.5 当前正式前端仍依赖运行目录的 `frontend/dist`

当前 `src/main.rs`：

```rust
QueryApi::router(
    AppContext { ledger, scanner },
    PathBuf::from("frontend/dist"),
)
```

这意味着当前正式运行隐含要求：

```text
当前工作目录
└─ frontend/dist
```

安装用户不应该知道 React/Vite，也不应该随安装包维护源码目录结构。

正式发行目标必须改成：

```text
React/Vite source
    ↓ npm run build
frontend/dist
    ↓ release build 时嵌入
Usagi binary
    ↓ runtime
Axum 从 binary 内部提供 index.html / JS / CSS / font
```

开发模式仍允许 Vite dev server；正式发布模式不得依赖 `frontend/dist` 的运行时磁盘位置。

---

## 1.6 当前启动器没有用户级生命周期处理

当前：

```rust
TcpListener::bind(address)
    .await
    .expect("could not bind Usagi to 127.0.0.1:3210");
```

结果：

```text
第二次启动 Usagi
或 3210 被其他软件占用
→ panic
```

发布版本必须区分：

```text
A. 3210 空闲
   → 正常启动

B. 3210 已经是 Usagi
   → 不创建第二套 Ledger / Scanner
   → 打开已存在的 Dashboard
   → 第二个启动进程正常退出

C. 3210 是其他软件
   → 不误判为 Usagi
   → 输出明确的端口占用错误
   → 不覆盖、不 kill 对方进程
```

---

## 1.7 当前没有更新服务

当前应用没有：

```text
current version API
GitHub Release 查询
4h update timer
manual update check
update UI state
release page opener
```

本轮新增，但必须与 Scanner / Ledger 生命周期隔离。

---

## 1.8 当前没有 `.github/workflows/`

因此目前没有：

```text
Windows 真环境自动构建
macOS arm64 自动构建
push / PR 自动测试
Tag 自动打安装包
GitHub Release 自动上传
```

公开仓库后，标准 GitHub-hosted runner 可以直接用于 public repository 的 CI/CD。

---

# 2. v0.1.0 最终运行架构

```text
GitHub Public Repository
│
├─ source
├─ docs
├─ tests
├─ .github/workflows/ci.yml
├─ .github/workflows/release.yml
└─ Releases
      ├─ Windows x64 installer
      ├─ macOS arm64 package
      └─ SHA256SUMS.txt

用户安装/启动 Usagi
        │
        ├─ 平台路径解析
        │    ├─ CODEX_HOME
        │    └─ mu.sqlite3
        │
        ├─ 绑定 127.0.0.1:3210
        │
        ├─ Ledger / Scanner
        │
        ├─ Axum API
        │
        ├─ embedded React assets
        │
        ├─ 异步 UpdateService
        │      ├─ 启动后立即检查
        │      └─ 每 4h 检查 GitHub latest release
        │
        └─ 打开默认浏览器
               ↓
       http://127.0.0.1:3210
               ↓
            Dashboard
               │
               └─ UpdateButton
                    ├─ 检查更新
                    ├─ 检查中…
                    └─ 版本升级
```

浏览器仍然只通过 Usagi 本机服务获得应用数据。

版本检查网络请求必须是：

```text
React
  ↓ same-origin
127.0.0.1:3210/api/update/*
  ↓
Rust UpdateService
  ↓ outbound HTTPS
api.github.com
```

禁止改成：

```text
React 浏览器
  ↓
api.github.com
```

这样可以保持现有 Dashboard 的本机同源安全边界。

---

# 3. Cargo / 依赖层调整

## 3.1 保留的核心原则

```text
Cargo.lock 必须提交
CI / Release 使用 --locked
不使用 vendor/
不要求最终用户安装 Cargo/Rust
```

## 3.2 建议新增依赖

具体 patch version 由 Luna 在施工时以当时 `cargo add` / crates.io 可解析的稳定版本写入 `Cargo.lock`，不得使用 Git branch dependency。

建议：

```text
directories
  平台 Home / 应用数据目录

rust-embed
  Release binary 内嵌 Vite dist

mime_guess
  embedded asset HTTP Content-Type

reqwest
  后端异步访问 GitHub REST API
  default-features = false
  使用 rustls TLS + json
  避免 OpenSSL 平台安装依赖

semver
  0.1.0 / v0.1.1 版本语义比较

webbrowser
  从 Rust 打开系统默认浏览器 / Release 页面

windows-sys（只在 cfg(windows) target dependency）
  Windows 稳定文件 Handle 信息读取
```

`blake3` 当前已经存在，可复用，不新增第二套 hash 库。

## 3.3 cargo-packager 的定位

`cargo-packager` 只作为**发布工具**使用，不进入 Usagi runtime dependency。

它负责：

```text
Windows → NSIS .exe installer
macOS   → .app / .dmg
```

因此安装器工具故障不得影响普通 `cargo test`、核心 Scanner 或 UpdateService。

---

# 4. Track A：平台路径抽象

## 4.1 新建平台层

建议新增：

```text
src/platform/
├─ mod.rs
├─ paths.rs
├─ file_identity.rs
└─ browser.rs
```

职责严格分开。

### `paths.rs`

负责：

```text
home_dir()
default_codex_home()
default_database_path()
路径 canonical / lexical normalize 的平台差异
```

### `file_identity.rs`

负责：

```text
Unix physical identity
Windows physical identity
mtime_ns
从 path metadata / open File handle 获得可比较身份
```

### `browser.rs`

负责：

```text
打开 http://127.0.0.1:3210
打开已验证 GitHub Release URL
```

业务模块不得各自写：

```text
$HOME
%USERPROFILE%
open
cmd /c start
std::os::unix::...
```

---

## 4.2 macOS 默认路径必须保持兼容

现有用户数据库：

```text
~/Library/Application Support/Usagi/mu.sqlite3
```

改造后仍必须解析到同一路径。

不得因为引入 `directories` 产生：

```text
~/Library/Application Support/<new-id>/...
```

从而让现有用户看见一个全新空数据库。

因此平台目录实现完成后必须先做“旧 macOS 默认路径等价 Gate”。

---

## 4.3 Windows 默认路径

目标：

```text
Codex Home:
<User Home>/.codex

Usagi DB:
Windows Local AppData 下 Usagi 的本地应用数据目录/mu.sqlite3
```

只要求路径符合 Windows 标准应用数据位置，不要求与 macOS 字符串结构一致。

`CODEX_HOME` 环境变量仍高于默认 `<home>/.codex`。

---

## 4.4 Windows canonical path 注意事项

Windows canonicalize 可能产生 verbatim path / UNC 形式。

Luna 必须保证：

1. 内部路径比较稳定；
2. 同一真实路径不会因为 `C:\...` 与 verbatim form 被误判成两个 Codex Home；
3. user-facing path 不出现不必要的奇怪前缀；
4. 禁止用简单字符串 replace 破坏合法 UNC / 长路径。

如果标准库输出不能同时满足以上要求，应把 Windows path simplification 放入 `platform::paths` 单点处理，不允许散落到 scanner/storage/frontend。

---

# 5. Track B：Windows 物理文件身份正式实现

这是 Windows 支持的最高优先级生产改造。

## 5.1 目标语义

macOS 当前依赖：

```text
device id + inode
```

Windows 必须获得等价的“同一台机器上稳定识别一个物理文件”的身份。

Windows 使用文件 Handle 信息中的：

```text
Volume Serial Number
+
File Index
```

作为原始身份来源。

不得继续使用 `(0,0)`。

---

## 5.2 为什么不直接使用稳定 std API

当前 Rust stable 的 `std::os::windows::fs::MetadataExt` 中，部分按 handle 提供 volume serial / file index 的 API 仍不是可依赖的 stable 接口。

因此本版 Windows 正式实现使用 Win32 `GetFileInformationByHandle`，通过 `windows-sys` 调用。

Windows 官方语义：Volume Serial Number 与 File Index 组合可以用于判断两个 Handle 是否代表同一文件。

---

## 5.3 与现有 SQLite schema 的兼容策略

当前 schema 和大量现有代码已经使用：

```text
device_id INTEGER CHECK >= 0
inode     INTEGER CHECK >= 0
```

本轮**不为 Windows 单独重写整个身份 schema**。

建议新增统一内部类型：

```text
PlatformFileIdentity
```

对 Unix：

```text
storage_part_a = device id
storage_part_b = inode
```

对 Windows：

```text
raw identity
= volume_serial_number + 64-bit file_index

↓ 使用现有 blake3 做稳定 hash

取两个非负 63-bit 整数
→ 写入现有 device_id / inode 两个 storage slot
```

目的：

1. 不改变现有数据库表结构；
2. Windows 不需要把 96-bit 原始 identity 强塞进一个 signed i64；
3. 保留两个持久化比较字段；
4. hash 输入只包含文件系统 identity，不包含路径，因此 rename 后仍保持同一物理身份语义；
5. 126-bit 有效身份空间足够把碰撞风险压到可忽略级别。

注意：数据库列名因为历史原因仍叫 `device_id/inode`；Windows 上它们是兼容 storage slots，不再字面表示 Unix dev/ino。代码注释必须写清楚，禁止后续开发者误用。

---

## 5.4 Discovery 改造

当前：

```text
Unix → stat_identity(metadata)
non-Unix → (0,0,size,mtime)
```

改成：

```text
Discovery
↓
symlink_metadata 检查
↓
确认 regular file
↓
platform::file_identity 获取真实 identity
↓
size + mtime_ns
↓
DiscoveredFile
```

Windows 为获得 handle identity 可以打开文件 Handle，但**不得读取 rollout 正文**。

Discovery 的核心原则仍然成立：

```text
枚举 + stat/handle metadata
不解析 JSONL 正文
```

身份读取失败：

```text
SOURCE_STAT_FAILED / 等价 privacy-safe diagnostic
```

不得降级成 `(0,0)`。

---

## 5.5 ChunkReader 改造

`src/scanner/chunk_reader.rs` 删除无条件 Unix `MetadataExt` import。

读前：

```text
path metadata
open File
handle identity
```

读取固定 view 后：

```text
同一 File handle identity
+ size
+ mtime
```

最后：

```text
重新检查 path
→ path 仍指向同一 physical identity
```

原有安全语义全部保留：

```text
SourceChangedBeforeRead
SourceChangedDuringRead
SourceSymlinkRejected
CheckpointGuardMismatch
```

不得因为 Windows API 麻烦而删除任何一层检查。

---

## 5.6 其它引用点统一迁移

检查并改造所有生产/测试中的：

```text
std::os::unix::fs::MetadataExt
.dev()
.ino()
.mtime()
.mtime_nsec()
```

生产代码必须走同一平台 helper。

测试代码中：

- 真正只验证 Unix 权限/Unix symlink 特性的测试可保留 `#[cfg(unix)]`；
- 验证 Usagi 通用物理身份语义的测试必须改成平台 helper，在 Windows 也运行；
- 不允许为了让 Windows CI 绿，把所有 scanner integration test 整体 `#[cfg(unix)]`。

特别核对：

```text
src/scanner/chunk_reader.rs
src/scanner/discovery.rs
src/scanner/mod.rs
src/scanner/usage_consumer.rs

tests/spec03_scanner_integration.rs
tests/spec04_usage_integration.rs
tests/spec06_frontend_browser.rs
```

---

# 6. Track C：正式前端静态资源嵌入

## 6.1 生产目标

正式安装后的 Usagi 不能依赖：

```text
cwd/frontend/dist
```

正式 release binary 必须自带：

```text
index.html
JS
CSS
fonts
其它 Vite assets
```

---

## 6.2 保留开发模式，区分“开发服务”和“发行服务”

开发仍允许：

```text
Terminal 1: cargo run
Terminal 2: npm run dev
```

正式发行：

```text
npm ci
npm run build
cargo build --release --locked --features embedded-frontend
```

建议新增 Cargo feature：

```text
embedded-frontend
```

理由：

- 普通 Rust 单元测试不应因为 `.gitignore` 中不存在 `frontend/dist` 而强制要求 Node；
- Release workflow 明确先 build frontend，再构建 embedded binary；
- Browser Gate 仍可测试 filesystem dev static 模式；
- 另补一个 embedded static Gate 验证正式发行路径。

这只是**静态资源来源的 build mode 差异**，不是数据逻辑的 dual-read/fallback。

---

## 6.3 API 层接口调整

当前：

```rust
QueryApi::router(context, static_dir)
```

建议收敛成明确的两种构造入口：

```text
QueryApi::router_with_embedded_frontend(context)
QueryApi::router_with_static_dir(context, path) // dev/test only
```

或内部等价抽象。

生产 `main.rs` 使用 embedded 入口。

必须保留现有路由语义：

```text
/api/*
→ API router
→ 未知 API 返回 JSON 404

其它页面路径
→ embedded static
→ SPA 深链接 fallback index.html
```

不得发生：

```text
未知 /api/foo
→ index.html
```

---

## 6.4 静态资源响应要求

至少保证：

```text
.html → text/html
.js   → 正确 JavaScript Content-Type
.css  → text/css
.woff2→ font/woff2
其它资源 → 合理 MIME
```

Vite hashed asset 可以使用浏览器缓存；`index.html` 不应被长期强缓存导致升级后仍引用旧 bundle。

不要求本轮增加复杂压缩/CDN 逻辑。

---

# 7. Track D：启动器、重复启动与默认浏览器

## 7.1 调整启动顺序

当前先打开 Ledger / Scanner，再 bind 端口。

发布版改成：

```text
1. resolve 127.0.0.1:3210
2. 尝试 bind
3. bind 成功
   → 再创建 Ledger
   → 再启动 Scanner
   → 再构造 Router
   → 启动 Axum
   → 自动打开浏览器

4. bind 失败 AddressInUse
   → probe 当前 3210 是否为 Usagi
```

这样第二次点击应用时不会先打开第二套 SQLite/Scanner。

---

## 7.2 给 `/api/health` 增加不可误判的应用标记

当前 `/api/health` 只返回 204。

增加固定 header，例如：

```text
X-Usagi-App: Usagi
X-Usagi-Version: 0.1.0
```

版本 header 取 `CARGO_PKG_VERSION`。

第二实例 probe：

```text
GET http://127.0.0.1:3210/api/health
Host 正确
无跨域 Origin
```

只有同时满足：

```text
成功响应
+
X-Usagi-App 精确匹配
```

才可认为端口上的服务是现有 Usagi。

---

## 7.3 重复启动行为

```text
端口已由 Usagi 占用
↓
打开 http://127.0.0.1:3210
↓
第二实例 exit 0
```

不得：

```text
启动第二个 scanner
修改 ledger
kill 第一个 Usagi
换随机端口启动第二套 MU
```

---

## 7.4 真实端口冲突

若 probe 不满足 Usagi marker：

```text
明确报告：127.0.0.1:3210 已被其他程序占用
```

不得自动 kill 进程，不得静默改端口，因为现有 Host/Origin 安全规则和测试明确依赖 3210。

---

## 7.5 自动打开浏览器

首次正常启动：

```text
http://127.0.0.1:3210
```

打开失败只属于 launcher 辅助能力失败：

```text
服务器继续运行
输出可手动打开的 URL
```

不得因为系统没有可调用默认浏览器而让 scanner / API 退出。

---

## 7.6 v0.1.0 进程生命周期边界

本版不增加 tray/service/daemon 生命周期系统。

特别是 Windows：**不要在没有“退出 Usagi”机制之前，仅为了隐藏窗口就盲目切换到完全不可见的 `windows_subsystem = "windows"` 后台进程。**

首版应优先保证：

```text
进程状态可理解
启动失败可诊断
用户仍有明确停止进程的方法
```

如果最终打包器默认行为会隐藏进程，则在 Packaging Gate 必须补充明确退出方式；否则保留前台进程行为，不在本轮引入 tray。

这项属于发布 UX Gate，不允许 Luna自行把进程变成不可退出的隐藏常驻程序。

---

# 8. Track E：后端 UpdateService

## 8.1 模块建议

新增：

```text
src/update/
├─ mod.rs
├─ github.rs
└─ state.rs
```

或者等价文件布局。

职责：

```text
state.rs
→ UpdateState / check lock / 状态快照

github.rs
→ GitHub latest release HTTP adapter

mod.rs
→ UpdateService orchestration / timer / public API
```

禁止把 GitHub HTTP 逻辑写进 `api.rs` handler。

---

## 8.2 UpdateService 状态

至少保存进程内状态：

```text
current_version
latest_version: Option<Version>
update_available: bool
release_url: Option<String>
last_successful_checked_at_ms: Option<i64>
last_attempted_at_ms: Option<i64>
checking: bool
```

不新增 SQLite migration。

更新状态没有必要跨应用重启持久化；应用启动会立即后台检查一次。

---

## 8.3 GitHub 仓库配置

只维护一个固定公开仓库：

```text
<github_owner>/<github_repo>
```

不得支持用户输入任意 update URL。

发布前将真实 repo 写入一个集中配置位置。

所有 update 代码只能从这个位置读取 repo 坐标，禁止：

```text
api.rs 一份 repo name
frontend 一份 repo name
release workflow 再一份不同 repo name
```

`release.yml` 可从当前 GitHub repository context 上传；Runtime UpdateService 需要固定仓库坐标。

---

## 8.4 GitHub latest release 请求

后端访问：

```text
GitHub REST: Get the latest release
```

公开仓库允许不带用户 GitHub Token 读取公开 Release。

HTTP client 要求：

```text
HTTPS
固定 User-Agent: Usagi/<current_version>
GitHub REST API version header
短超时（建议总请求 5s 量级）
不读取无关仓库内容
不携带用户 Codex/OpenAI 信息
```

响应只取：

```text
tag_name
published release identity
```

Release URL建议根据已验证的固定 repo + tag 自己构造，而不是盲目信任任意外部 URL 字段。

---

## 8.5 版本比较

规则：

```text
current = CARGO_PKG_VERSION，例如 0.1.0
latest tag = v0.1.1
```

处理：

```text
去掉可选前缀 v
semver::Version::parse
```

结果：

```text
latest > current
→ update_available = true

latest == current
→ false

latest < current
→ false，绝不提示降级

invalid tag
→ 本次检查失败
→ 不覆盖上一份成功状态
```

首版只发布 stable Release，不把 prerelease 当作正式升级目标。

---

## 8.6 自动检查调度

核心启动流程：

```text
Listener / Ledger / Scanner / Router 正常可用
↓
spawn UpdateService background task
↓
立即进行第一次 check
↓
每 4 小时再次 check
```

关键硬约束：

**主程序启动不得 await GitHub 检查完成。**

即：

```text
GitHub DNS 卡住
GitHub timeout
电脑离线
公司网络屏蔽 GitHub
GitHub 5xx
```

均不得阻塞：

```text
Dashboard
Scanner
SQLite
SSE
手动刷新
Session Drawer
```

自动检查失败：

```text
静默记录状态
不弹前端错误
4h 后继续
```

---

## 8.7 并发检查去重

可能出现：

```text
自动 4h check 正在执行
同时用户点击“检查更新”
```

UpdateService 必须保证同一时刻最多一个 GitHub 网络检查。

建议：

```text
一个 async check Mutex / Semaphore
```

后来的手动请求可等待当前检查完成，或复用同一结果；不得同时发两份完全相同 GitHub 请求。

这只锁 UpdateService，不得持有 Ledger/Scanner mutex。

---

## 8.8 失败状态不能抹掉已知新版

例如：

```text
12:00 成功检查：发现 v0.1.1
16:00 GitHub 网络失败
```

16:00 后仍应：

```text
update_available = true
latest_version = 0.1.1
按钮 = 版本升级
```

失败只更新：

```text
last_attempted_at
last attempt result
```

不能把上一份成功结果清空。

---

# 9. Track F：Update API

在现有 `/api` 下新增：

```text
GET  /api/update/status
POST /api/update/check
POST /api/update/open-release
```

全部继续经过现有 `local_request_guard`。

---

## 9.1 `GET /api/update/status`

只读进程内状态，不访问 GitHub。

返回示例：

```json
{
  "current_version": "0.1.0",
  "latest_version": "0.1.1",
  "update_available": true,
  "last_checked_at_ms": 1786680000000,
  "checking": false
}
```

前端轮询这个 endpoint 的成本必须接近普通内存读取。

注意：不要把 GitHub Release URL 作为前端必须直接导航的信任输入；推荐前端通过 `/open-release` 让后端打开已验证地址。

---

## 9.2 `POST /api/update/check`

用途：用户主动点击“检查更新”。

建议继续要求：

```text
X-Usagi-Request: 1
```

与 `/api/refresh` 的本机主动操作约定一致。

行为：

```text
await 本次 UpdateService check
```

这里“await”只影响该 HTTP request 和更新按钮，不影响 Dashboard 其它 API/Scanner。

成功返回最新状态。

失败返回明确 Update API error，例如：

```text
UPDATE_CHECK_FAILED
```

前端提示“检查更新失败，请稍后重试”。

---

## 9.3 `POST /api/update/open-release`

只允许在后端已有：

```text
update_available = true
+
valid latest version
```

时执行。

后端构造：

```text
https://github.com/<owner>/<repo>/releases/tag/vX.Y.Z
```

然后调用平台默认浏览器。

前端只调用本机 endpoint，不自行 fetch GitHub。

如果打开浏览器失败：

```text
返回明确错误
但不改变 update_available
```

---

# 10. Track G：前端“检查更新 / 版本升级”按钮

## 10.1 文件布局

建议：

```text
frontend/src/dashboard/UpdateButton.tsx
frontend/src/dashboard/useUpdateController.ts
```

更新 API 类型/方法放入现有：

```text
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts
```

不得把 fetch 直接散落在 `DashboardPage.tsx`。

---

## 10.2 UI 位置

放在当前 Dashboard Header 右侧操作区，和：

```text
上次更新
同步按钮
```

同一操作区域管理。

建议顺序：

```text
[检查更新 / 版本升级]  上次更新：xx  [同步]
```

不要改变 KPI、筛选器、Session 布局。

---

## 10.3 状态机

### 状态 A：默认

```text
按钮：检查更新
```

### 状态 B：用户主动检查中

```text
按钮：检查中…
按钮自身 disabled
```

只禁用这个按钮。

以下功能继续工作：

```text
日期筛选
模型筛选
项目筛选
同步
Session 表
Drawer
SSE
```

### 状态 C：主动检查，已是最新

```text
按钮恢复：检查更新
提示：当前已是最新版本 v0.1.0
```

使用短时 Toast / inline feedback，不使用阻塞 Modal。

### 状态 D：主动检查，发现新版

例如：

```text
current 0.1.0
latest  0.1.1
```

UI：

```text
提示：发现新版本 v0.1.1
按钮：版本升级
```

### 状态 E：自动检查发现新版

```text
不弹窗
不自动跳转
不打断当前操作
按钮自动变成：版本升级
```

### 状态 F：用户点击“版本升级”

```text
POST /api/update/open-release
↓
系统默认浏览器打开对应 GitHub Release
```

按钮仍保持 `版本升级`。

### 状态 G：自动检查失败

```text
无 Toast
无 Modal
不修改当前正常 UI
```

### 状态 H：用户主动检查失败

```text
按钮恢复原可用状态
提示：检查更新失败，请稍后重试
```

如果之前已经知道有新版：

```text
按钮仍保持 版本升级
```

---

## 10.4 前端如何感知后端 4h 自动检查结果

不修改现有 revision SSE 语义。

不要把“版本更新事件”硬塞进当前只处理 data/status revision 的 `/api/events`。

首版使用轻量本机状态轮询：

```text
Dashboard load
→ GET /api/update/status

之后每 60 秒
→ GET /api/update/status
```

这个请求只读 UpdateService 内存，不访问 GitHub。

所以：

```text
GitHub 每 4h 请求一次
前端每 60s 只查询 localhost 状态
```

自动发现新版后，页面最长约 60 秒把按钮更新为 `版本升级`。

该方案比新增第二套 SSE 简单，并且不会干扰现有 revision feed。

---

# 11. Track H：README 与公开仓库整理

## 11.1 README 必须重写过时内容

当前 README 仍写：

```text
Usagi 是 macOS 浏览器工具
Dashboard 界面仍由后续 Spec 实现
```

与当前代码不符。

新 README 至少包括：

```text
Usagi 是什么
支持平台
Windows/macOS 安装方式
启动方式
默认本机 URL
默认 CODEX_HOME
默认数据库位置
数据只在本机处理的说明
更新检查行为
每 4h 后端检查 + 手动检查
不自动安装更新
macOS 未签名提示
从源码构建
开发环境要求（Rust + Node）
最终用户不需要 Rust/Node
测试命令
```

---

## 11.2 公开前清理本机信息

搜索：

```text
<repo>
~/... 或 C:\Users\<user>\...
个人临时绝对路径
token / api key / PAT / secret
.env
私有 fixture
```

文档里的本机路径改成：

```text
<repo>
~/...
C:\Users\<user>\...
```

如果实际 Git 历史中曾提交秘密，仅删除当前文件不够；公开前必须检查历史。

Luna 不得把真实凭据写入：

```text
yml
Cargo.toml
README
source
release config
```

---

## 11.3 开源文件

公开前至少：

```text
README.md
LICENSE
.gitignore
Cargo.lock
```

可选但建议后续再补：

```text
CONTRIBUTING.md
SECURITY.md
```

它们不是 v0.1.0 功能 Gate，不要阻塞本轮施工。

---

# 12. Track I：CI（`.github/workflows/ci.yml`）

## 12.1 CI 定位

CI = 每次 push / Pull Request 自动在 GitHub runner 上构建并执行关键测试。

当前最重要价值：**用户没有长期 Windows 开发环境，但 Windows 兼容不能靠 macOS 推测。**

公开仓库使用标准 GitHub-hosted runners。

---

## 12.2 CI 触发

```yaml
on:
  push:
  pull_request:
```

不在普通 CI 自动创建 Release。

---

## 12.3 建议拆成三个 Job

### Job 1：frontend

Runner：

```text
ubuntu-latest
```

执行：

```text
checkout
setup-node
npm ci
npm test
npm run build
```

产出 `frontend/dist` 可作为后续 embedded build artifact，也可以各平台自己重新 build；首版优先简单，不强行做复杂 artifact 共享。

### Job 2：Rust / platform matrix

至少：

```text
windows-latest
macos-latest        // arm64
```

每个平台：

```text
checkout
setup Rust stable
cargo fmt --check（可集中只跑一次）
cargo test --locked
```

Windows 必须实际运行 scanner/file identity 相关跨平台测试，不只是 `cargo check`。

### Job 3：embedded release build smoke

在目标平台：

```text
npm ci
npm run build
cargo build --release --locked --features embedded-frontend
```

然后从一个**不包含 `frontend/dist` 的临时运行目录**启动构建后的 binary，验证：

```text
/api/health 可访问
/ 返回 Dashboard index
assets 可返回
```

这条 Gate 专门证明：正式 binary 已经不依赖工作目录的 `frontend/dist`。

---

## 12.4 Clippy 处理原则

本轮不要为了“CI 看起来漂亮”无范围扩张清理历史上所有无关 lint。

如果当前 baseline 已经能：

```text
cargo clippy --all-targets -- -D warnings
```

则加入硬 Gate。

如果 baseline 仍存在已知无关 clippy debt：

1. 先记录 baseline；
2. 本轮新增/修改文件不得增加 warning；
3. 不在发布方案内擅自重构 unrelated cost/codex 模块；
4. 是否把全仓 clippy 升为 Release Gate，等测试标准阶段统一确定。

---

# 13. Track J：Release workflow

新增：

```text
.github/workflows/release.yml
```

---

## 13.1 只由正式 Tag 触发

```text
v*.*.*
```

例如：

```text
git tag v0.1.0
git push origin v0.1.0
```

---

## 13.2 Release 前置版本一致性 Gate

Workflow 必须先验证：

```text
Git tag v0.1.0
↓ strip v
0.1.0
==
Cargo.toml package.version 0.1.0
```

不一致直接 fail。

不得出现：

```text
binary 0.1.0
Release v0.1.1
```

---

## 13.3 Windows x64 build

Runner：

```text
windows-latest
```

执行：

```text
npm ci
npm run build
cargo test --locked
release build embedded frontend
```

Windows release build 采用 MSVC x64。

目标是最终用户不手工安装 VC++ Redistributable。

优先策略：Release build 启用静态 CRT：

```text
-C target-feature=+crt-static
```

但只在 Windows release target 生效，不全局污染 macOS/测试配置。

Build 后检查动态依赖，禁止意外依赖：

```text
VCRUNTIME*.dll
MSVCP*.dll
```

如果某第三方 crate 与 static CRT 冲突，Luna 必须报告并给出证据，不得简单删除“用户无需 VC runtime”目标。

---

## 13.4 macOS build

一个 native runner：

```text
macos-latest   → arm64
```

各自：

```text
npm ci
npm run build
cargo test --locked
cargo build --release --locked --features embedded-frontend
```

本版：

```text
不 codesign
不 notarize
```

---

## 13.5 安装包

使用 cargo-packager：

```text
Windows:
Usagi-v0.1.0-windows-x64-setup.exe

macOS arm64:
Usagi-v0.1.0-macos-arm64.dmg
```

禁止把源码目录或 Node/Rust runtime 打进安装包。

React 已在 executable 内，不需要把 `frontend/` 作为 runtime resource 整目录复制。

---

## 13.6 Release assets

最终 GitHub Release 至少：

```text
Usagi-v0.1.0-windows-x64-setup.exe
Usagi-v0.1.0-macos-arm64.dmg
SHA256SUMS.txt
```

GitHub 自己会提供 source code archive，不需要 Luna 再重复上传源码 zip。

---

## 13.7 上传 Release

同一 public repository 中完成。

Workflow 使用仓库内置 `GITHUB_TOKEN`，只给 release job：

```yaml
permissions:
  contents: write
```

不得创建并硬编码个人 PAT。

普通 CI job 使用最小权限。

---

# 14. 安装后的用户体验

## 14.1 Windows

```text
用户下载
Usagi-v0.1.0-windows-x64-setup.exe
↓
安装
↓
从开始菜单/快捷方式启动 Usagi
↓
Usagi 解析 %USERPROFILE% 对应 Home/.codex
↓
打开 Local AppData 中自己的 mu.sqlite3
↓
监听 127.0.0.1:3210
↓
打开默认浏览器
↓
Dashboard
```

用户不需要知道：

```text
Cargo
Rust
Vite
npm
frontend/dist
SQLite CLI
```

---

## 14.2 macOS

```text
下载 macOS arm64 dmg
↓
安装/运行 Usagi
↓
由于本版未签名，系统可能需要用户手动允许首次启动
↓
Usagi 继续读取
~/.codex
以及原有
~/Library/Application Support/Usagi/mu.sqlite3
↓
浏览器打开 Dashboard
```

---

# 15. 更新机制完整时序

## 15.1 后端自动检查

```text
Usagi 启动
↓
Core ready
↓
spawn update task
↓
立即检查 GitHub latest release
↓
成功：缓存结果
失败：静默
↓
4h
↓
再次检查
↓
循环
```

如果用户关闭 Usagi：

```text
不再检查
```

本版不注册系统后台服务。

---

## 15.2 页面自动感知

```text
Dashboard load
↓
GET /api/update/status
↓
每 60s 再读一次 localhost 状态
```

不等于每 60s 请求 GitHub。

真正 GitHub 请求仍然只有后端 4h 自动检查和用户手动检查。

---

## 15.3 用户主动检查

```text
检查更新
↓ click
检查中…
↓
POST /api/update/check
↓
Rust -> GitHub
```

结果 1：

```text
current == latest
→ 当前已是最新版本 v0.1.0
→ 检查更新
```

结果 2：

```text
latest > current
→ 发现新版本 v0.1.1
→ 版本升级
```

结果 3：

```text
network/API fail
→ 检查更新失败，请稍后重试
→ 不影响其它页面功能
```

---

## 15.4 自动发现新版

```text
后台 4h check
↓
v0.1.1 > v0.1.0
↓
UpdateService cache update_available=true
↓
前端下一次 /api/update/status
↓
检查更新 → 版本升级
```

无强制弹窗。

---

## 15.5 用户升级

```text
版本升级
↓ click
POST /api/update/open-release
↓
Rust 打开固定仓库对应 v0.1.1 Release 页面
↓
用户自行下载安装新版
```

本版到此结束。

绝对不做：

```text
自动下载 exe/dmg
自动执行 installer
程序自覆盖
未经用户确认重启
```

---

# 16. Luna 具体施工顺序

下面顺序是本方案的正式施工顺序。除标明可并行的步骤外，Luna 不得自行大范围重排。

---

## S0：冻结代码基线与发布参数

只读确认：

```text
Cargo.toml version = 0.1.0
当前 git HEAD
当前 macOS 默认 DB path
当前 CODEX_HOME
当前 test baseline
```

记录但不修改业务数据口径。

创建本轮工作分支，例如：

```text
release/v0.1.0-distribution
```

**Gate S0：** baseline 可复现；没有把未提交业务改动混入发布改造。

---

## S1：仓库 / Cargo 发布基础清理

修改：

```text
Cargo.toml
Cargo.lock
.cargo/config.toml
.gitignore
```

动作：

1. 删除 vendored source replacement；
2. 清理 `.cargo/config.toml.saved`；
3. 添加本方案确定的 runtime dependencies / target dependency；
4. 保持 `Cargo.lock`；
5. 所有构建使用 crates.io + lockfile；
6. 先不要加入 GitHub workflow，以免基础代码未跨平台就持续红。

**Gate S1：** macOS 本机可以从非 vendor 依赖完成 `cargo check/test --locked`；前端 `npm ci` 正常。

---

## S2：平台路径层

新增 `src/platform/paths.rs` 等基础模块。

修改 `src/storage/mod.rs` 默认路径解析。

先只完成路径，不碰文件 identity。

**Gate S2：** macOS 原 DB path 100% 不变；Windows 单元测试能解析 Home/.codex 和 Local AppData 路径。

---

## S3：跨平台文件身份层

这是 Windows 主 Track。

按第 5 章完成：

```text
platform file identity
Discovery
ChunkReader
相关 scanner production call sites
```

删除 Windows `(0,0)` 占位实现。

**Gate S3：** Windows `cargo test --locked` 能编译并执行 scanner 核心 identity 测试；替换/变化检测语义保持。

> S3 未通过前，禁止进入“Windows 已支持”结论，也禁止先打 installer。

---

## S4：跨平台测试代码适配

集中处理：

```text
std::os::unix test imports
ps RSS P2 test
Unix-only permission/symlink tests
```

原则：

```text
通用语义 → 跨平台运行
真正 OS 特有语义 → cfg(target) + 对应平台验证
```

Windows `ps` 不存在，P2 RSS 采样需抽象为平台实现，或把 RSS 采样器做平台-specific adapter；不得用“Windows 直接跳过整个资源 Gate”替代。

**Gate S4：** Windows test target 不再因为 Unix import/command 失败。

---

## S5：嵌入式前端

完成：

```text
embedded-frontend feature
embedded static service
SPA fallback
MIME
main.rs production static source
```

开发模式保持。

**Gate S5：** release binary 拷贝到没有 `frontend/dist` 的临时目录仍可完整打开 Dashboard。

---

## S6：启动器 / 重复实例 / 浏览器

调整 `main.rs` 启动顺序。

完成：

```text
先 bind
health marker
已有 MU probe
真实端口冲突
默认浏览器打开
```

**Gate S6：** 连续启动两次不会创建第二 scanner；第二次只拉起现有页面并成功退出。

---

## S7：UpdateService core

新增 update 模块，但先不接 UI。

完成：

```text
GitHub adapter
semver compare
UpdateState
single-flight check
启动后台 immediate + 4h timer
失败隔离
```

使用可注入的 GitHub adapter / HTTP seam，测试不得依赖真实 GitHub 网络才能 PASS。

**Gate S7：** mocked latest release 下可证明 newer/equal/older/error 的状态转换；GitHub error 不影响 Ledger/Scanner。

---

## S8：Update API

修改：

```text
AppContext
api router
query/error types（如需要）
```

接入：

```text
GET  /api/update/status
POST /api/update/check
POST /api/update/open-release
```

**Gate S8：** API 契约稳定后冻结，前端开始接入；不得在之后随意改字段名。

---

## S9：前端更新按钮

新增：

```text
UpdateButton
useUpdateController
client methods / types
UI style
```

实现第 10 章状态机和 60s localhost status poll。

**Gate S9：** 手动最新/新版/失败 + 自动新版四种核心 UI 行为正确；其它 Dashboard 控件不被检查过程禁用。

---

## S10：README / 公开仓库清理

更新 README，清理私人路径，确认 `.gitignore`。

增加用户确认后的 LICENSE。

**Gate S10：** 新用户只看 README 可以完成 Windows/macOS 安装和源码构建；公开仓库无明显本机隐私/secret。

---

## S11：GitHub CI

新增 `.github/workflows/ci.yml`。

先让：

```text
Windows
macOS arm64
frontend
```

全部跑通。

**Gate S11：** GitHub Actions 真机 matrix 全绿；不是只在本机 Mac 通过。

---

## S12：Release workflow + 安装包

新增：

```text
.github/workflows/release.yml
packager config
```

完成：

```text
tag/version match
Windows x64 release
mac arm64 release
SHA256
GitHub Release upload
```

先用测试 tag / draft Release 做 dry run，不直接把失败产物标记为正式 v0.1.0。

**Gate S12：** 两个目标安装包均由 GitHub runner 产出，并能执行安装/启动 smoke。

---

## S13：最终 v0.1.0 发布 Gate

只有全部前置 Gate 通过后：

```text
Cargo.toml = 0.1.0
↓
git tag v0.1.0
↓
release workflow
↓
GitHub Release v0.1.0
```

发布后再用已发布的 v0.1.0 做一次真实 `latest release` 检查。

然后用一个临时测试构建版本，例如内部测试 current `0.0.9`，验证能发现 public v0.1.0；不得为了测试污染正式 Release tag。

---

# 17. 并行施工协调

为了减少总时间，可以并行，但只允许在文件所有权清晰时并行。

## 17.1 S1 完成后可并行的第一组

```text
Track A
S2 + S3 + S4
平台路径 / Windows identity / test platformization

Track B
S5
embedded frontend
```

冲突控制：

```text
Track A 不改 frontend
Track B 不改 scanner/storage
```

`Cargo.toml` 依赖必须已在 S1 集中处理，避免两个 Luna 同时改 dependency block。

---

## 17.2 第一组完成后统一集成

```text
S2-S5
↓
Integration Gate I
↓
统一 main/api build
```

S6 由单一施工者完成，因为会集中修改 `main.rs`。

---

## 17.3 S6 后可并行第二组

```text
Track C
S7 UpdateService core

Track D
S10 README / repository cleanup
```

两者文件基本不重叠。

---

## 17.4 Update API 契约冻结后

```text
S8 完成并冻结 API
↓
Track E: S9 Frontend
Track F: S11 CI skeleton
```

S11 在 S9 合入后再追加最终 frontend/update tests。

---

## 17.5 最后必须串行

```text
S12 Release workflow
↓
S13 final release
```

安装包和正式 Tag 不能并行“边改边发”。

---

# 18. Luna 施工 + Gate 总图

```text
┌──────────────────────────────────────────────┐
│ S0 冻结当前功能基线 / 记录 v0.1.0           │
└───────────────────────┬──────────────────────┘
                        ↓
                 [Gate 0 Baseline]
                        ↓
┌──────────────────────────────────────────────┐
│ S1 Cargo / vendor / dependency 发布基础      │
└───────────────────────┬──────────────────────┘
                        ↓
                 [Gate 1 Build Base]
                        ↓
          ┌─────────────┴─────────────┐
          ↓                           ↓
┌─────────────────────┐     ┌──────────────────────┐
│ S2-S4 Platform Track│     │ S5 Embedded Frontend │
│ Paths               │     │ Vite dist -> binary  │
│ Windows Identity    │     │ SPA fallback         │
│ Cross-platform tests│     └──────────┬───────────┘
└──────────┬──────────┘                │
           └──────────────┬─────────────┘
                          ↓
              [Gate I Cross-platform Core]
                          ↓
┌──────────────────────────────────────────────┐
│ S6 Launcher                                  │
│ bind-first / duplicate instance / browser    │
└───────────────────────┬──────────────────────┘
                        ↓
                [Gate 6 Runtime]
                        ↓
          ┌─────────────┴─────────────┐
          ↓                           ↓
┌──────────────────────┐    ┌──────────────────────┐
│ S7 UpdateService     │    │ S10 Repo / README    │
│ GitHub / 4h / state  │    │ public-ready cleanup │
└──────────┬───────────┘    └──────────┬───────────┘
           ↓                           │
      [Gate 7 Update Core]             │
           ↓                           │
┌──────────────────────┐               │
│ S8 Update API        │               │
└──────────┬───────────┘               │
           ↓                           │
     [API Contract Freeze]             │
           ↓                           │
    ┌──────┴───────────┐               │
    ↓                  ↓               │
┌───────────────┐  ┌────────────────┐  │
│ S9 Frontend   │  │ S11 CI         │  │
│ Update Button │  │ Win/mac matrix │  │
└───────┬───────┘  └───────┬────────┘  │
        └──────────┬────────┴───────────┘
                   ↓
          [Gate II Full CI Green]
                   ↓
┌──────────────────────────────────────────────┐
│ S12 Release workflow + packager              │
│ Win x64 + mac arm64 + SHA256                 │
└───────────────────────┬──────────────────────┘
                        ↓
            [Gate III Install/Launch Smoke]
                        ↓
┌──────────────────────────────────────────────┐
│ S13 Public GitHub Release v0.1.0             │
└───────────────────────┬──────────────────────┘
                        ↓
                 [FINAL Gate]
                        ↓
                 v0.1.0 可分发
```

---

# 19. 每个最终 Gate 必须证明什么

## Gate A：不破坏现有 macOS 用户

```text
原数据库路径不变
原 CODEX_HOME 行为不变
现有 Usage/Session/cost 数据不重建成错误口径
现有 Dashboard 功能不回退
```

---

## Gate B：Windows 是真实支持，不是“能编译”

必须证明：

```text
真实 Windows runner 编译
真实 Windows runner scanner tests
真实 Windows file identity 非 (0,0)
两个不同 rollout identity 不相同
同一文件重复扫描 identity 稳定
文件被替换时 ChunkReader 能拒绝
Windows 路径正确
```

---

## Gate C：安装用户没有开发环境依赖

必须证明正式 artifact 不要求：

```text
Rust
Cargo
Node
npm
SQLite CLI
Visual Studio
Windows SDK
```

Windows 还应证明没有意外的 VC Redistributable 前置要求。

---

## Gate D：正式 binary 自包含前端

在没有 repo、没有 `frontend/dist` 的目录中：

```text
启动 binary
→ /api/health PASS
→ / PASS
→ JS/CSS/font PASS
→ Dashboard 可用
```

---

## Gate E：更新服务不是主程序依赖

模拟：

```text
GitHub timeout
DNS error
500
invalid JSON
invalid tag
```

均不得影响：

```text
启动
Scanner
Ledger
API
Dashboard
手动同步
```

---

## Gate F：前端更新行为完全符合需求

```text
常态 检查更新
主动 checking 检查中…
最新版 toast + 恢复检查更新
新版 → 版本升级
后台新版 → 版本升级且不打断
主动失败 → 提示失败
后台失败 → 静默
版本升级 → 打开正确 Release
```

---

## Gate G：发布版本身份一致

```text
Cargo 0.1.0
binary health/version 0.1.0
Tag v0.1.0
Release v0.1.0
asset filename v0.1.0
```

五处必须一致。

---

# 20. 预计主要修改文件

## Rust

```text
Cargo.toml
Cargo.lock
src/lib.rs
src/main.rs
src/api.rs
src/api/query.rs（如新增 Update ApiError）
src/storage/mod.rs
src/scanner/discovery.rs
src/scanner/chunk_reader.rs
src/scanner/mod.rs（平台 identity call site）
src/scanner/usage_consumer.rs（如 identity type 收敛）

新增：
src/platform/mod.rs
src/platform/paths.rs
src/platform/file_identity.rs
src/platform/browser.rs
src/update/mod.rs
src/update/github.rs
src/update/state.rs
src/api/static_assets.rs（或等价）
```

## Frontend

```text
frontend/src/dashboard/DashboardPage.tsx
frontend/src/index.css
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts

新增：
frontend/src/dashboard/UpdateButton.tsx
frontend/src/dashboard/useUpdateController.ts
相应 test 文件
```

## Tests

重点核对：

```text
tests/spec02_metadata_integration.rs
tests/spec03_scanner_integration.rs
tests/spec04_usage_integration.rs
tests/spec06_frontend_browser.rs
src/scanner/chunk_reader/tests/spec04_p2.rs
```

后续测试标准文档会决定具体新增测试文件落点；本方案阶段不得随意把所有发布测试塞进旧 Spec 文件。

## Repository / Release

```text
README.md
LICENSE（用户确认后）
.gitignore
.cargo/config.toml
.github/workflows/ci.yml
.github/workflows/release.yml
packager config
```

---

# 21. 施工禁止项

Luna 执行时明确禁止：

1. 为 Windows 直接删除 `device_id/inode` identity 检查；
2. 保留 Windows `(0,0)` 作为正式实现；
3. 为了 Windows CI 通过把整个 Scanner 测试套件 `#[cfg(unix)]`；
4. 把 GitHub Token / PAT 写进 Usagi；
5. React 直接请求 `api.github.com`；
6. GitHub 检查失败时阻塞 `main()` 启动；
7. 自动检查失败后清空之前已经发现的新版状态；
8. 自动下载或自动执行新版安装包；
9. 改成 Tauri/Electron；
10. 改随机端口规避 3210 冲突；
11. 第二实例 kill 第一实例；
12. 把 `frontend/dist` 作为正式安装目录依赖；
13. 为解决 `.cargo/config` 问题提交巨大 `vendor/`；
14. 让最终用户安装 Rust/Node/SQLite；
15. 因本轮发布需求修改 Usage/Session/cost 业务口径；
16. 未通过 Windows 真环境 Gate 就宣称“支持 Windows”；
17. 未经用户确认擅自选择开源许可证；
18. 未完成 dry run 就直接创建不可撤回意义上的正式 v0.1.0 发布流程。

---

# 22. 最终完成定义（Definition of Done）

只有同时满足以下条件，本轮才算完成：

```text
[ ] 当前 macOS 数据路径/业务行为保持兼容
[ ] Windows x64 能真实构建和运行
[ ] Windows 使用真实物理文件 identity
[ ] macOS arm64 能构建
[ ] React 静态资源已嵌入正式 binary
[ ] 正式运行不需要 frontend/dist
[ ] 启动后自动打开 Dashboard
[ ] 重复启动不 panic
[ ] 非 MU 的 3210 占用能明确识别
[ ] UpdateService 启动后异步检查
[ ] 自动更新检查周期 4h
[ ] GitHub 失败不影响主程序
[ ] Dashboard 有“检查更新”按钮
[ ] 主动检查最新版有明确提示
[ ] 发现新版按钮变“版本升级”
[ ] 自动发现新版不强制打断用户
[ ] “版本升级”打开正确 GitHub Release
[ ] 不做自动下载安装
[ ] CI 在 Windows/macOS 真 runner 全绿
[ ] Tag 与 Cargo version 有自动一致性检查
[ ] GitHub Release 自动生成两个正式安装包
[ ] Release 有 SHA256SUMS
[ ] README 已改为 Windows + macOS 当前事实
[ ] 公共仓库不存在明显 secret / 私人绝对路径
[ ] LICENSE 已由用户确认
[ ] 正式 GitHub Release v0.1.0 可供普通用户下载
```

---

# 23. 实施完成后的下一步

本实施方案审核通过后，再单独建立：

```text
Usagi_v0.1.0_跨平台分发与更新机制测试标准_v0.1.md
```

测试标准应只覆盖本轮新增风险：

```text
Windows 路径/文件身份
macOS 回归
embedded frontend
launcher / duplicate instance
UpdateService
UpdateButton
CI / Release artifact
clean-machine install smoke
```

不重复重新设计已经由现有测试标准覆盖的 Token / Session / cost 全量业务测试。

正式测试标准确定后，再回看本文 S0-S13 顺序和并行 Track，必要时只调整 Gate 协调，不改变已审核的功能范围。

---

# 24. 技术依据备注

本方案涉及的外部机制采用以下官方/一手语义：

- Rust Cargo：`cargo vendor` 是把 crates.io/Git 依赖复制到本地 vendor 目录；本项目无离线构建要求，因此不需要 vendor replacement。
- Rust/Cargo：Cargo 是构建和依赖管理工具，编译后的原生 executable 不要求最终用户安装 Cargo/Rust。
- Windows：`GetFileInformationByHandle` / `BY_HANDLE_FILE_INFORMATION` 的 Volume Serial Number + File Index 可用于比较文件物理身份。
- GitHub：公开仓库的 Latest Release REST endpoint 可以匿名读取公开 Release。
- GitHub Actions：公开仓库使用 standard GitHub-hosted runners 可用于 Windows/macOS CI。
- GitHub 2026 public runners：`windows-latest` 为 x64；`macos-latest` 为 arm64。
- `rust-embed`：用于在编译时把 CSS/JS/images 等静态文件嵌入 Rust executable。
- `cargo-packager`：支持 Windows NSIS installer 与 macOS app/dmg；本方案只把它作为 release tooling，不加入运行时逻辑。
