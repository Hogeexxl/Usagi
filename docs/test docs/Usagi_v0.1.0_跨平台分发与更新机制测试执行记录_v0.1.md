# Usagi v0.1.0 跨平台分发与更新机制测试执行记录 v0.1

> 验收依据：`docs/test docs/Usagi_v0.1.0_跨平台分发与更新机制测试标准_v0.1.md`  
> 正式版本：`v0.1.0`  
> 正式 Release commit：`a3c3f7d8e27cecdf6f66157540b84e878532e2a0`  
> 正式 tag：`v0.1.0`（annotated tag，指向上述 commit）  
> 记录日期：2026-08-16  
>
> 本记录只记录实际执行结果、实际 GitHub Actions run、实际 Release / artifact 与最终 PASS/FAIL。测试标准正文中的“待实现/未进行”是制定时状态，不代表本执行记录的最终结果。

---

## 1. 最终结论

按正式测试标准完成最终收口：

```text
T-DIST-001 ～ T-DIST-016：16 / 16 PASS
T-DIST-017：1 / 1 PASS
P0：0 FAIL
P1：0 FAIL
P2（本文登记项）：0 FAIL
Gate A ～ Gate G：全部 PASS
FINAL Gate：PASS
```

因此：**Usagi v0.1.0 跨平台分发与更新机制正式完成。**

正式支持范围保持为：

```text
Windows 10/11 x64 installer
macOS Apple Silicon arm64 DMG
SHA256SUMS.txt
```

macOS Intel/x86_64 不属于 v0.1.0 正式范围，不作为 FAIL。

---

## 2. 正式版本身份

### 2.1 Release commit / tag

正式发布代码：

```text
a3c3f7d8e27cecdf6f66157540b84e878532e2a0
release: add S13 public update and FINAL validation
```

正式 annotated tag：

```text
v0.1.0
```

GitHub tag object 最终解析到：

```text
v0.1.0
  -> a3c3f7d8e27cecdf6f66157540b84e878532e2a0
```

后续 `main` 上的验证脚本修复不移动 `v0.1.0` tag，也不改变已经发布的 v0.1.0 产品二进制。

### 2.2 Cargo / binary / Release 版本

Release workflow 对正式 tag 执行了版本一致性检查：

```text
Cargo.toml package version = 0.1.0
stable tag              = v0.1.0
binary reported version = 0.1.0
GitHub Release           = v0.1.0
asset filename version   = v0.1.0
```

结果：PASS。

---

## 3. GitHub Actions 正式证据

### 3.1 正式 tag CI

Run：`31914667763`  
Event：push tag `v0.1.0`  
Commit：`a3c3f7d8e27cecdf6f66157540b84e878532e2a0`  
结论：**success**

实际成功 jobs：

```text
Rust format
Embedded release smoke (Windows x64)
Embedded release smoke (macOS arm64)
Rust (Windows x64)
Rust (macOS arm64)
Frontend
```

其中实际执行并通过：

```bash
cargo fmt --check
cargo check --locked
cargo test --locked

npm ci
npm run test
npm run check
npm run build
```

Windows x64 job 还实际执行了 distribution identity/scanner evidence；两个平台均实际执行 embedded release runtime smoke。

该 run 作为 T-DIST-001～011 与 FINAL 回归的重要统一真环境证据。

### 3.2 正式 Release workflow

Run：`31914667748`  
Commit/tag：`a3c3f7d...` / `v0.1.0`  
结论：**success**

Jobs：

```text
Windows x64 release        job 95084678303  PASS
macOS arm64 release        job 95084678204  PASS
Publish GitHub Release     job 95086169322  PASS
```

关键实际步骤：

```text
Windows：
- locked Rust tests
- embedded release build
- PE imports / subsystem inspection
- NSIS installer build
- T-DIST-013 clean-runtime installer smoke

macOS：
- Apple Silicon runner architecture verification
- locked Rust tests
- arm64 embedded release build
- binary architecture inspection
- unsigned DMG build
- T-DIST-014 clean-runtime arm64 DMG smoke

Publish：
- tag / Cargo version recheck
- 下载两个平台 artifact
- 生成并校验 SHA256SUMS
- 发布正式 GitHub Release
```

结果：T-DIST-012～014 PASS。

### 3.3 S13 / FINAL Recovery

Run：`31935694770`  
最终 attempt：2  
结论：**success**

实际成功 jobs：

```text
T-DIST-015 public Release update E2E recovery       job 95139677321
T-DIST-016 frontend regression recovery             job 95139905199
T-DIST-016 Rust recovery (macOS arm64)              job 95139905224
T-DIST-016 Rust recovery (Windows x64)              job 95139905312
T-DIST-017 affected RSS P2 recovery (Windows x64)   job 95139905245
T-DIST-017 affected RSS P2 recovery (macOS arm64)   job 95139905275
Release quality Clippy evidence recovery            job 95139905211
Automated FINAL evidence summary recovery           job 95140748250
```

最终 automated summary：PASS。

说明：首次 post-release 验证暴露的是 S13 验证脚本的工作目录耦合，不是 v0.1.0 产品 runtime 缺陷。修复只涉及验证基础设施；修复后的 recovery run 重新对**已发布的 v0.1.0 commit / DMG / public Release**取得正式证据。

### 3.4 Public Release checksum asset repair

Run：`31937397761`  
Job：`95141359376`  
结论：**success**

稳定 Release 发布后最终 API 复核一度发现公开资产列表缺少 `SHA256SUMS.txt`。为避免把 workflow 中“曾生成 checksum”误当成最终公开状态，执行了独立修复：

```text
1. 从公开 v0.1.0 Release 重新下载 Windows EXE 与 macOS DMG
2. 对真实公开二进制重新计算 SHA-256
3. sha256sum --check PASS
4. 上传 SHA256SUMS.txt 到现有 v0.1.0 Release
5. 再次读取公开 Release，强制断言资产集合恰好为 3 个正式资产
```

最终结果：PASS。

---

## 4. T-DIST-001～017 最终映射

| ID | 最终结果 | 主要实际证据 |
|---|---|---|
| T-DIST-001 | PASS | 正式 tag CI fresh checkout；Windows/macOS `cargo check/test --locked`；Frontend `npm ci` |
| T-DIST-002 | PASS | 两平台无过滤 Rust 测试 + macOS/Windows runtime/release smoke；平台 path 逻辑随正式 suite 通过 |
| T-DIST-003 | PASS | Windows x64 CI 的 distribution identity/scanner evidence 实际执行并成功；无 `(0,0)` 正式 fallback |
| T-DIST-004 | PASS | Windows x64 真 runner `cargo test --locked` + scanner evidence PASS |
| T-DIST-005 | PASS | Windows/macOS `Embedded release smoke` 均 PASS；正式 binary 在 release 路径运行，不依赖源码 `frontend/dist` |
| T-DIST-006 | PASS | 启动器/端口/重复实例行为包含于正式无过滤 Rust suite；两平台正式 runtime smoke PASS |
| T-DIST-007 | PASS | UpdateService 状态机/调度/隔离测试包含于正式无过滤 Rust suite并通过 |
| T-DIST-008 | PASS | Update API contract / HTTP 安全相关测试包含于正式无过滤 Rust suite并通过 |
| T-DIST-009 | PASS | Frontend 完整 `npm run test/check/build` PASS；更新按钮/controller 测试未被过滤 |
| T-DIST-010 | PASS | Gate E 已关闭；最终仓库为公开仓库，README/LICENSE/公开边界保持发布状态 |
| T-DIST-011 | PASS | CI run `31914667763` 同一 release commit 上 Windows x64、macOS arm64、Frontend、embedded smoke 全绿 |
| T-DIST-012 | PASS | Release run `31914667748` 版本一致性与 checksum 生成通过；最终公开 checksum asset 经 repair run 再确认 |
| T-DIST-013 | PASS | Windows x64 release job `95084678303` 的 clean-runtime installer smoke PASS |
| T-DIST-014 | PASS | macOS arm64 release job `95084678204` 的 clean-runtime DMG smoke PASS |
| T-DIST-015 | PASS | Recovery job `95139677321` 对真实 public latest + 已发布 DMG 完成 E2E |
| T-DIST-016 | PASS | Recovery frontend + Windows Rust + macOS Rust 全量回归 jobs 均 PASS |
| T-DIST-017 | PASS | Windows/macOS 两个平台的 1 GiB chunk-reader/RSS P2 jobs 均 PASS |

---

## 5. T-DIST-015 真实公开更新检查

实际命令入口：

```bash
bash ./.github/scripts/s13-public-release-e2e.sh
```

Recovery run 实际输出：

```text
Anonymous public latest Release verified: v0.1.0
Released Usagi 0.1.0 correctly reports current/latest equality
Internal Usagi 0.0.9 correctly detects public 0.1.0 as an update
T-DIST-015 PASS
```

实际验证语义：

```text
公开 latest Release = v0.1.0
正式 released binary current = 0.1.0
latest = 0.1.0
update_available = false
release_url = 正确 v0.1.0 Release URL

内部 runner-only build current = 0.0.9
latest = 0.1.0
update_available = true
release_url = 同一个公开 v0.1.0 Release URL
```

测试在客户端检查阶段不需要 GitHub Token；低版本只在 runner 工作区临时构建，没有创建低版本 tag/Release，也没有污染正式发布历史。

Gate G：PASS。

---

## 6. T-DIST-016 FINAL 全量回归

FINAL Recovery 对正式 release commit 重新执行，而不是复用早期 Gate 的旧成功结果。

### Frontend

```bash
npm ci
npm run test
npm run check
npm run build
```

结果：PASS。

### Windows x64

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
```

无测试名过滤、无 `--skip`。结果：PASS。

### macOS arm64

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
```

无测试名过滤、无 `--skip`。结果：PASS。

T-DIST-016：PASS。

---

## 7. T-DIST-017 P2 资源复核

Windows x64 与 macOS arm64 均执行：

```bash
cargo test --locked \
  t_s04_052_one_gib_bounded_reader_keeps_batches_and_process_memory_bounded \
  -- --ignored --nocapture
```

两个平台 job 均 PASS。

该测试使用实际 1 GiB rollout / chunk-reader 路径与平台 RSS sampler，复用既有预算，没有因 v0.1.0 发布工程放宽指标。

T-DIST-017：PASS。

---

## 8. Clippy 发布质量检查

实际执行：

```bash
cargo clippy --all-targets -- -D warnings
```

该命令本身返回 exit code 101，命中的 warning/error 位于既有代码，例如：

```text
src/cost/mod.rs
src/cost/estimator.rs
src/cost/pricing.rs
src/codex/metadata.rs
src/codex/rollout.rs
src/scanner/coordinator.rs
src/storage/usage/tests/spec04_p2.rs
```

典型类别包括：unused imports / dead code / collapsible_if / question_mark / needless borrow。

按正式测试标准 Clippy 规则，本轮不得为了让历史 warning 消失而扩大范围清理无关业务代码。S13 recovery 修复只修改验证脚本，不新增 Rust 产品 warning；因此这些 warning 记录为**既有 baseline 发布质量债务**，不作为本轮新增 FAIL，也不改变 T-DIST-016 / FINAL 的 PASS 判定。

本记录不把 `continue-on-error` 冒充为“Clippy 命令 PASS”；实际状态明确保留为：

```text
formal clippy command: exit 101
classification: pre-existing unrelated baseline warnings
new S13/FINAL Rust warning: none introduced
FINAL blocking result under documented rule: no
```

---

## 9. 最终公开 GitHub Release

Release：`Usagi v0.1.0`  
Release ID：`371185376`  
URL：`https://github.com/Hogeexxl/Usagi/releases/tag/v0.1.0`  
状态：

```text
draft      = false
prerelease = false
public latest = v0.1.0
anonymous access = PASS
```

### 最终正式资产

| Asset | 最终公开 SHA-256 |
|---|---|
| `Usagi-v0.1.0-windows-x64-setup.exe` | `6b70cebcb63378690000fe4ca5b8a5428de733f299aa2fc83af7f2b0e7fd4624` |
| `Usagi-v0.1.0-macos-arm64.dmg` | `b39a6dda5480a83c04121bfb0dd39b8abef35c8c5524a4b0933b408e2c235f0d` |
| `SHA256SUMS.txt` | GitHub asset 已公开；文件内容包含上述两个安装资产的真实 SHA-256 |

Repair run 对公开安装资产实际执行：

```text
Usagi-v0.1.0-windows-x64-setup.exe: OK
Usagi-v0.1.0-macos-arm64.dmg: OK
```

`SHA256SUMS.txt` 内容中的两条值为：

```text
6b70cebcb63378690000fe4ca5b8a5428de733f299aa2fc83af7f2b0e7fd4624  Usagi-v0.1.0-windows-x64-setup.exe
b39a6dda5480a83c04121bfb0dd39b8abef35c8c5524a4b0933b408e2c235f0d  Usagi-v0.1.0-macos-arm64.dmg
```

最终资产集合验证为恰好：

```text
Usagi-v0.1.0-macos-arm64.dmg
Usagi-v0.1.0-windows-x64-setup.exe
SHA256SUMS.txt
```

---

## 10. Packaging / Runtime 实际结果

### Windows x64

正式 installer 来自 Release workflow，实际执行并通过：

```text
NSIS installer build
PE dependency / subsystem inspection
clean-runtime installer smoke
health + embedded Dashboard runtime verification
```

结果：PASS。

最终用户运行不要求预装：

```text
Rust
Cargo
Node.js
npm
SQLite CLI
Visual Studio
Windows SDK
```

### macOS arm64

正式 DMG 来自 Release workflow，实际执行并通过：

```text
Apple Silicon runner architecture check
arm64 release binary inspection
unsigned DMG build
clean-runtime arm64 DMG smoke
health + embedded Dashboard runtime verification
```

结果：PASS。

v0.1.0 仍按既定范围：unsigned / not notarized；这不是 FAIL。

---

## 11. 发布过程中的实际修复记录

最终发布阶段出现过两项**验证/交付基础设施问题**，均已闭环并保留真实记录：

1. S13 public Release E2E 首次执行暴露验证脚本工作目录耦合。修复后重新对已发布 v0.1.0 执行 recovery，T-DIST-015～017 与 automated FINAL 全部 PASS；未修改 v0.1.0 产品 runtime。
2. 正式 Release workflow 曾生成并校验 `SHA256SUMS.txt`，但后续最终公开 API 复核时该文件不在资产列表。独立 repair run 从**实际公开的两个安装资产**重新计算 checksum、校验并上传，最终公开资产集合恢复为要求的 3 个文件。

没有通过：

```text
移动/覆盖 v0.1.0 tag
重发不同产品二进制
跳过失败测试
放宽断言
自动下载/静默升级
扩大范围清理历史 Clippy warning
```

来制造 PASS。

---

## 12. Gate 收口

最终状态：

```text
Gate A — Build Base          PASS
Gate B — Platform Core       PASS
Gate C — Runtime             PASS
Gate D — Update              PASS
Gate E — Public / CI         PASS
Gate F — Packaging           PASS
Gate G — Published Release   PASS
FINAL                        PASS
```

对应正式条目：

```text
T-DIST-001 ～ T-DIST-017：17 / 17 PASS
FAIL：0
未进行：0
```

**Usagi v0.1.0 已达到 `Usagi_v0.1.0_跨平台分发与更新机制测试标准_v0.1.md` 的最终完成定义。**
