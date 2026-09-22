# Usagi 同步失败二次调研报告

- 调研时间：2026-09-22
- 仓库：`/Users/hogee/Desktop/Usagi`
- 分支：`codex/spec01-multi-source-core`
- 目标数据库：`~/Library/Application Support/Usagi/mu.sqlite3`
- 运行版本：`0.2.6`
- 报告性质：只读取证 + 临时本机 error-chain 诊断；临时诊断代码已移除

## 1. 结论

这次问题**不是同步按钮没有生效**。

按钮对应的 HTTP 请求能够成功进入后台扫描：

```text
POST /api/refresh
X-Usagi-Request: 1
→ HTTP 202 Accepted
→ disposition: started
→ 生成新的 scan_id
```

但后台扫描随后在 Codex metadata commit 阶段失败：

```text
antigravity → completed
codex       → failed: METADATA_COMMIT_FAILED
```

当前已抓到的真正底层错误是：

```text
Thread antigravity:01762bcb-ea69-44e0-94d4-9cb6f8f4c6ed
canonical identity is immutable
```

也就是说：

> Codex metadata pipeline 正在尝试用 `source=codex` 更新一个已经存在且属于 `source=antigravity` 的 Thread。

这是一个**跨 source 的 metadata 范围泄漏**，不是按钮问题，也不是本轮真实数据库的缺失 root 问题。

## 2. 按钮和后台扫描取证

健康检查：

```text
GET /api/health
→ HTTP 204 No Content
x-usagi-app: Usagi
x-usagi-version: 0.2.6
```

按钮等价请求：

```text
POST /api/refresh
X-Usagi-Request: 1
→ HTTP 202 Accepted
scan_id: 5dd1936d-0ae1-446e-9d0b-6a5133664a41
```

该 scan 后续状态：

```text
antigravity → completed
codex       → failed / METADATA_COMMIT_FAILED
```

扫描大约在启动后数秒结束为失败。之后再次启动、手动刷新，失败均稳定复现。

最近一次失败 scan 的记录：

```text
scan_id:       169756d9-f8f0-4d9b-a2a4-9e39bfa54753
trigger:       Startup
state:         failed
error_code:    SOURCE_RUN_FAILED
```

## 3. 为什么 UI 显示昨天，但今天的 session 已经能看到

截图中的时间是：

```text
上次同步：23:17:21
```

数据库中的最近一次成功 scan 完成时间为：

```text
2026-09-21 23:17:21 +0800
```

这正好对应 UI 显示的时间。

之后的扫描虽然失败，但 metadata pipeline 是**按 Thread group 分组、逐组提交**的：

1. 前面的 Codex groups 先提交成功；
2. 到第 `1323` 个 group 时遇到 Antigravity Thread；
3. 该 group 失败；
4. 之前已经提交的 groups 不会回滚；
5. 整体 scan 被标记为 failed。

因此出现了看似矛盾的状态：

- UI 的“上次同步”仍然显示最近一次**完整成功**的 scan，即昨天 23:17:21；
- 今天的 session 已经出现在数据库中；
- `scan_state` 仍然是 `failed`。

数据库现场数据：

```text
Codex threads:                 1323
Antigravity threads:             34
Codex source files:            1331
今天目录下的 Codex files:        6
今天文件 metadata checkpoint ready: 6
最新 metadata checkpoint:       2026-09-22 18:42:34 +0800
```

这说明“今天的 session 能看到”并不能证明整轮同步成功；本次是**部分提交成功后，后续 group 失败**。

## 4. 底层 error chain

临时打开本机环境变量诊断后，捕获到的第一个失败 group 是：

```text
group_index=1323
thread_id=antigravity:01762bcb-ea69-44e0-94d4-9cb6f8f4c6ed
```

错误链：

```text
CodexStorageError::Storage
└── StorageError { kind: InvalidState }
    └── Thread antigravity:01762bcb-ea69-44e0-94d4-9cb6f8f4c6ed canonical identity is immutable
```

这与当前数据库中的多 source 数据完全吻合。

## 5. 代码根因

### 5.1 Codex ingestion 读取了所有 source 的 Thread

`src/codex/ingestion/mod.rs:135-150`：

```rust
let existing_threads = storage
    .load_existing_threads()
    ...;
```

`src/codex/storage/metadata.rs:28` 的 SQL 当前是：

```sql
SELECT ...
FROM threads
ORDER BY thread_id
```

这里没有：

```sql
WHERE source = 'codex'
```

所以 Codex pipeline 会收到 Antigravity rows。

### 5.2 Resolver 将所有已有 Thread 都标记为 affected

`src/codex/metadata.rs:323-325`：

```rust
for thread in self.input.existing_threads.clone() {
    self.affected.insert(thread.thread_id.clone());
    self.existing.insert(thread.thread_id.clone(), thread);
}
```

当前 `ExistingThread` projection 又没有保留 source 字段，因此后续 resolver 无法再做 source 过滤。

### 5.3 Resolver 对每个 Thread 都构造 Codex identity

`src/codex/metadata.rs:754`：

```rust
let identity = SessionIdentity::new(thread_id, SourceId::CODEX, thread_id).ok()?;
```

因此当 `thread_id` 属于 Antigravity 时，resolver 仍会生成 `source=codex` 的 patch。

### 5.4 Storage 正确拒绝跨 source identity 覆盖

`src/codex/storage/metadata.rs:790-798` 的 `apply_existing_patch()` 检查：

```rust
if patch.source != current.source
    || patch.native_session_id != current.native_session_id
{
    return Err(StorageError::invalid_state(format!(
        "Thread {} canonical identity is immutable",
        patch.thread_id
    )));
}
```

这个拒绝行为本身是正确的；错误在于 Codex pipeline 不应该把 Antigravity Thread 放进自己的 commit batch。

### 5.5 外层仍然把具体错误压缩成通用码

`src/codex/ingestion/mod.rs:158`：

```rust
.map_err(|_| "METADATA_COMMIT_FAILED")?;
```

所以 API 最终只能显示：

```text
METADATA_COMMIT_FAILED
```

## 6. 已排除的候选根因

当前真实数据库检查结果：

```text
source=codex
active_epoch=1
build_epoch=NULL
active_parser_version=11
```

因此本次不是残留 `build_epoch` 导致的 usage build reconciliation。

关系完整性检查：

```text
missing parent rows: 0
missing root rows:   0
```

当前 active usage 指向缺失 root 的数量也是 `0`。此前的 root materialization 修复仍然有价值，但不是这次真实失败的首要触发点。

## 7. 建议修复方案

### P0：让 Codex metadata 查询只返回 Codex Threads

最小修复：修改 `load_existing_threads()` 查询：

```sql
FROM threads
WHERE source = 'codex'
ORDER BY thread_id
```

这样 Antigravity rows 不会进入 Codex resolver 或 Codex metadata commit batch。

### P0：增加 source invariant

建议同时让 `ExistingThread` 保留 `source` 字段，并在 pipeline/resolver 边界增加断言或过滤：

```rust
if thread.source != SourceId::CODEX {
    continue;
}
```

仅依赖 SQL 过滤不够稳妥，因为未来可能有其他构造 `ResolutionInput` 的调用者。

### P1：新增 mixed-source 回归测试

至少覆盖：

1. `threads` 同时存在 Codex 和 Antigravity rows；
2. Codex metadata resolution 运行；
3. Antigravity row 不得进入 Codex commit batch；
4. Codex scan 成功；
5. Antigravity row 的 canonical identity 不发生变化。

建议固定当前真实失败 Thread 的脱敏 fixture，验证错误：

```text
canonical identity is immutable
```

修复后不再出现。

### P1：修正失败后的可观测性

保留：

- `scan_id`
- `group_index`
- `source`
- `thread_id`
- commit stage
- 完整 `Error::source()` chain

对外 API 仍可返回 `METADATA_COMMIT_FAILED`，但后台诊断不能丢失具体 source/thread。

### P2：处理部分提交语义

当前 group-isolated commit 会导致：

```text
scan_state = failed
但数据库已经部分前进
```

这不是本次失败的直接原因，但会造成 UI 时间与实际数据时间不一致。修复 source 泄漏后，应进一步决定：

- 是否让成功的 group 进度继续保留并明确标记 partial；或
- 在一个完整 source scan 内统一事务；或
- 在 status 中同时显示“最后成功扫描时间”和“最后数据进度时间”。

## 8. 最终判断

- 同步按钮：**正常**，请求返回 `202` 并成功启动后台扫描。
- 后端调度：**正常**，Antigravity 能完成，Codex 能进入 ingestion。
- 数据库物理完整性：此前检查为正常。
- 当前实际根因：**Codex metadata pipeline 混入了 Antigravity existing Thread，并用 Codex identity 更新它**。
- 今天 session 能看到：因为失败发生在后面的 group，前面的 group 已经部分提交。
- UI 显示昨天 23:17:21：因为那是最近一次完整成功 scan 的完成时间，而今天的 scan 全部失败。

本报告只记录调查结果；本次没有把 P0 source filter 修复直接写入代码，也没有修改或删除用户数据库。
