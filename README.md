# Usagi

> 在本机查看 Codex 的 Token、会话与预估费用。

![Backend](https://img.shields.io/badge/backend-Go-00ADD8)
![Data](https://img.shields.io/badge/data-Magpie-orange)
![Frontend](https://img.shields.io/badge/frontend-React-61dafb)
![License](https://img.shields.io/badge/license-MIT-green)

Usagi 是一个本地 Codex 用量 Dashboard。Go runtime 通过 Magpie Web/Gateway 获取数据，将结果保存在本机 SQLite 中，并向浏览器 Dashboard 与 Codex Plugin 提供同一套本地 HTTP API。Dashboard 支持 Token、费用和会话概览，以及时间范围、模型和项目筛选。

## 支持平台

- macOS Apple Silicon arm64
- Windows 10/11 x64

从 [GitHub Releases](https://github.com/Hogeexxl/Usagi/releases) 下载对应文件，实际可用版本以 Release 页面为准：

| 平台 | Runtime | Codex Plugin |
| --- | --- | --- |
| macOS arm64 | `Usagi-v<VERSION>-macos-arm64.dmg` | `Usagi-Codex-Plugin-v<VERSION>-macos-arm64.zip` |
| Windows x64 | `Usagi-v<VERSION>-windows-x64-setup.exe` | `Usagi-Codex-Plugin-v<VERSION>-windows-x64.zip` |

macOS 安装包内包含 `Usagi.app`。Windows 使用 NSIS 安装器，按当前用户安装到 `%LOCALAPPDATA%\Programs\Usagi`，主程序为 `usagi.exe`，并创建 Start Menu 快捷方式和卸载程序。

## 启动与数据目录

Usagi 只监听 `127.0.0.1` 回环地址，并在启动后自动打开系统默认浏览器中的 Dashboard。正式版使用 `37891–37922` 端口段，开发版使用 `37991–38022` 端口段；当端口被其他程序占用时会继续尝试后续端口，若已有同模式 Usagi 在运行，再次启动只会打开现有 Dashboard。

Windows 版启动后在后台持续运行，没有长期命令行窗口、原生主窗口或系统托盘常驻图标。关闭浏览器不会停止 runtime。

| 数据 | macOS | Windows |
| --- | --- | --- |
| SQLite | `~/Library/Application Support/Usagi/usagi.sqlite` | `%LOCALAPPDATA%\Usagi\usagi.sqlite` |
| 正式版描述符 | `~/Library/Application Support/Usagi/runtime.json` | `%LOCALAPPDATA%\Usagi\runtime.json` |
| 开发版描述符 | `~/Library/Application Support/Usagi/runtime-dev.json` | `%LOCALAPPDATA%\Usagi\runtime-dev.json` |

描述符提供当前 Runtime 的 `base_url`。如果浏览器没有自动打开，可连接该地址访问 Dashboard。

## Magpie

Magpie Web/Gateway 是 Usagi 的实时同步数据源。进行实时同步时 Magpie 必须可用，但 Magpie 不是 Usagi process 的启动前置条件：

- Usagi installer 不打包、安装或更新 Magpie。
- Magpie 未运行或启动失败时，Usagi process、Dashboard 与 SQLite 仍会启动，已有数据仍可读取；Supervisor 会进入 unavailable 状态并在后台重试。
- 新数据库在首次成功同步前保持 loading/unavailable 语义，Magpie 恢复后无需重启 Usagi。

## Codex Plugin

Codex Plugin 是独立于 Runtime 的下载项，不包含、替代或启动 Usagi。使用时需要已有一个正在运行的 production Usagi；Plugin 从本机 `runtime.json` 发现动态 localhost endpoint，并在 descriptor 缺失或 identity 校验失败时返回 not-running/invalid 状态。

Browser Dashboard 与 Codex Plugin 使用同一套 Dashboard 业务代码和 Usagi HTTP API。

## 开发

开发需要 Go `1.27.1`、Node.js `22` 与 `npm`。在仓库根目录可用以下命令：

```bash
npm --prefix frontend ci
npm --prefix frontend test
npm --prefix frontend run check
go test ./...
go vet ./...
```

设置 `USAGI_DISABLE_BROWSER=1` 可禁止启动默认浏览器，适用于自动化测试和本地开发。

## License

[MIT](LICENSE)
