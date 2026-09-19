# 📊 Usagi

> 完全运行在本机的 Codex 用量仪表盘：Token、会话与预估费用，一屏看清。

![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS-blue)
![Backend](https://img.shields.io/badge/%E5%90%8E%E7%AB%AF-Rust-orange)
![Frontend](https://img.shields.io/badge/%E5%89%8D%E7%AB%AF-React-61dafb)
![License](https://img.shields.io/badge/license-MIT-green)

## 什么是 Usagi

Usagi 是一个纯本机运行的 Rust 服务 + React Dashboard，用来读取 OpenAI Codex CLI / Codex Desktop 留在本机的会话记录（rollout JSONL），整理出 Token 用量、预估费用与Session详细数据。

它有三个原则：

- **只读**：只读取 Codex 数据目录，绝不修改、绝不删除 Codex 的任何文件；
- **本地**：数据处理、SQLite 数据库、Dashboard 页面全部留在你的电脑上，服务只监听 `127.0.0.1` 回环地址，不监听外网；macOS 固定使用 `3210`，Windows 优先使用 `3210`，冲突时自动选择后续本机端口；
- **无上传**：不会把会话内容、用量数据或 OpenAI 凭据发送到任何服务器。唯一的网络请求是向 GitHub 公开接口查询新版本，且不携带任何本地数据。

## 功能一览

### 🖥 数据面板

启动后自动打开浏览器进入 Dashboard，提供 8 张 KPI 卡：

| 指标 | 说明 |
| --- | --- |
| 预估费用 | 按内置价格表估算的美元费用，附数据完整性提示 |
| 总 Token | 范围内所有会话的 Token 总量 |
| 输入 / 输出 Token | 输入与输出分开统计，输出含推理（reasoning）部分 |
| 会话数量 | 范围内的 Session 数 |
| 缓存命中率 | 命中缓存的部分占输入的比例 |
| 缓存读取 Token | 命中缓存的输入量 |
| 推理 Token | 输出中用于推理的部分 |

### 📋 Session 列表与详情

- 会话表格支持按最后活动时间、项目、模型、Token 量、费用排序，分页浏览；
- 点开任一会话进入详情抽屉：主会话用量按「模型 + reasoning effort」分块展示，包含输入、输出、缓存读写、命中率、费用等 8 项明细；
- 自动识别 Subagent 会话，列表中可逐个展开其独立用量；表格中的「合计」列（Token、费用）为包含 Subagent 的口径，与主会话自身的「总计」列分开呈现。

### 🔍 筛选与时间范围

- 时间范围：今天 / 昨天 / 本周（周一起始）/ 本月 / 今年，按本机时区计算；
- 模型筛选：多选，GPT 系模型自动分组；
- 项目筛选：按项目多选，可单独包含或排除「无项目会话」「未识别项目」。

### 💰 费用估算

- 内置各模型单价表（输入 / 缓存读取 / 缓存写入 / 输出，含长上下文阶梯价），逐事件计算后汇总；
- 未知模型不会套用其他模型的价格，明确标记为「未知」；
- 费用为估算值，仅供参考，不代表 OpenAI 账单。详见[数据口径与费用估算](#数据口径与费用估算)。

### ⚡ 增量扫描与实时更新

- 启动时立即扫描一次，之后每 5 分钟增量扫描；也可以随时点「同步数据」手动触发；
- 增量扫描只读取文件新增部分（按上次读取的断点续读），文件被改写时自动校验并重建，无需人工干预；
- 服务端数据有变化时通过 SSE 推送修订号，Dashboard 自动刷新，不用手动刷新页面。

## 快速开始

### 前置条件

- 本机已安装并在使用 Codex CLI 或 Codex Desktop（Usagi 读取它们写下的 `~/.codex` 会话记录，自身不产生数据）；
- 操作系统：Windows 10/11（x64）或 macOS（Apple Silicon）。

### 安装

从 [GitHub Releases](https://github.com/Hogeexxl/Usagi/releases) 下载对应平台的安装包（以 Releases 页面实际提供的文件为准）：

- Windows x64：`Usagi-v0.2.6-windows-x64-setup.exe`，安装后的主程序为 `usagi.exe`
- macOS Apple Silicon：`Usagi-v0.2.6-macos-arm64.dmg`

启动后 Usagi 会在本机启动服务并自动打开默认浏览器；如果浏览器没有自动打开，手动访问：

```text
http://127.0.0.1:3210
```

### 常见问题

- **macOS 首次启动被拦截**：v0.1.0 的 macOS 应用未做 Developer ID 签名与 notarization。在 Finder 中按住 Control 点按应用并选择「打开」，或在「系统设置 → 隐私与安全性」中选择「仍要打开」。
- **重复启动**：再次启动只会打开已在运行的实例，不会启动第二个。
- **端口被占用**：Windows 会从 `3210` 开始自动选择后续可用的本机端口，不会结束或替换占用端口的程序；macOS 仍保持固定 `3210` 的原有行为。
- **看不到数据**：确认本机 `~/.codex/sessions` 下存在 `rollout-*.jsonl` 文件；如果设置过 `CODEX_HOME` 环境变量，确认它指向 Codex 实际使用的数据目录。数据每 5 分钟自动扫描一次，也可以点「同步数据」立即扫描。

## 数据来源

Usagi 只读取 Codex 的本地数据目录，不访问网络获取用量。目录按以下优先级解析：显式配置 > 环境变量 `CODEX_HOME`（非空时生效）> 默认 `~/.codex`。

| 文件 | 读取方式 | 用途 |
| --- | --- | --- |
| `$CODEX_HOME/sessions/**/rollout-*.jsonl` | 逐行流式读取 | Token 用量、turn、模型、reasoning effort |
| `$CODEX_HOME/archived_sessions/**/rollout-*.jsonl` | 同上 | 已归档会话的用量 |
| `$CODEX_HOME/state_5.sqlite` | 只读打开 | 会话元数据：标题、项目、模型、Subagent 派生关系 |
| `$CODEX_HOME/session_index.jsonl` | 逐行流式读取 | 会话标题等索引信息 |
| `$CODEX_HOME/.codex-global-state.json` | 只读 | Codex Desktop 的项目归属信息 |

扫描时的安全约束：

- 跳过所有符号链接，只接受常规文件；
- 以文件的物理身份（设备 + inode）跟踪文件，改名、移动不触发重扫，截断或改写自动重建；
- 只消费以换行符结尾的完整行，写入一半的行留待下一轮扫描，不会解析到脏数据。

## 本地数据与配置

Usagi 自身的数据全部落在本机：

| 数据 | 位置 |
| --- | --- |
| SQLite 数据库 | macOS：`~/Library/Application Support/Usagi/mu.sqlite3`<br>Windows：`%LOCALAPPDATA%\Usagi\mu.sqlite3` |
| Codex 数据目录（只读） | `$CODEX_HOME`，默认 `~/.codex`（Windows：`C:\Users\<user>\.codex`） |

可用的环境变量：

| 变量 | 作用 | 默认 |
| --- | --- | --- |
| `CODEX_HOME` | Codex 数据目录（非空时生效） | `~/.codex` |
| `TZ` | 时间范围计算使用的时区 | 系统时区 |

监听地址始终固定为 `127.0.0.1`，不会暴露到网络。macOS 继续固定使用 `3210`；Windows 优先使用 `3210`，若被其他程序占用则自动选择后续可用端口。

## Public API v1

Usagi 提供只读、版本化的本机 Public API，供 PeekFlow 等同机应用读取已经处理好的 Usage、Codex Quota 与状态数据。Public API 与 Dashboard 自用的 Internal API 共用底层 Query / Aggregate 能力，但拥有独立稳定契约。

Public API 根路径：

~~~text
http://127.0.0.1:3210/api/v1
~~~

首版提供 info、revision、revision SSE、status、Codex quota 与 usage summary；Usage 时间范围支持 today、yesterday、7d、30d、year 和 custom。

完整架构、接口表、SSE/GET 协作方式与时间语义见 [Public API v1 产品需求文档](docs/Usagi_Public_API_v1_产品需求文档.md)。

## 平台、更新与开发

- 正式发布支持：`Windows 10/11 x64` 与 `macOS Apple Silicon arm64`。
- Usagi 会在后台`每 4 小时`查询一次 GitHub Releases；也可以在 Dashboard 点击`检查更新`。发现新版本时会提供`版本升级`入口。
- 正式安装包是自包含应用；普通用户`不需要安装 Rust`，也不需要单独安装`Node.js`、`SQLite`或`Visual Studio`。
- 只有从源码开发或执行仓库验收时才需要相应开发工具。前端测试使用 `npm run test`，Rust 全量测试使用 `cargo test --locked`。

## License

[MIT](LICENSE)
