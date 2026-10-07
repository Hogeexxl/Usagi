# Codex Compaction Phase 5 验收记录

记录日期：2026-10-07  
范围：仅记录本需求 Phase 5 门禁、分阶段验收状态、fixture 证据与 §8.2 人工验收状态。实施方案及 fixture 未在本记录更新中修改。

## 用户最终验收与提交授权（2026-10-07）

用户完成手工测试后明确反馈“测试通过，提交一次，推送到远程”。§8.2 人工验收按用户反馈记为通过；下文“尚未执行”均为此前阶段的历史状态。本次仅授权将已验收改动合并为一次 Git 提交并推送当前分支，不包含版本发布或合并到主分支。

## Metadata no-op 签名扫描优化（2026-10-07）

`metadata_group` 在 metadata 写入前计算稳定字段变化；`commit_group` 先校验整组 source precondition 并计算 binding 变化 ID，再执行 binding 写入。只有稳定字段未变且 binding 确有变化时，才在写入前后比较原有全局 Compaction visibility signature。稳定字段变化直接报告可见变化；无 binding 变化的 fact/checkpoint 更新跳过全局扫描，事务校验与写入顺序保持原有语义。

| 定向用例 | 结果 |
| :--- | :--- |
| `codex::storage::metadata::tests::tests::source_only_repeat_does_not_advance_data_revision` | 1 passed，0 failed，exit 0；包含 `parser_version=12`、`processing_status=ready` 的 usage checkpoint；此 fixture 不断言 active epoch/parser。 |
| `codex::storage::metadata::tests::tests::spec04_first_binding_reconciles_build_in_same_metadata_transaction` | 1 passed，0 failed，exit 0；binding 与 build reconciliation revision 仍为 0。 |

完整日志曾保存在 `target/compaction-oversized-fix/`。代码验收仅运行上述两个 exact case，没有重跑完整 Gate；本次部署与一次正常 refresh 的现场结果记录如下。

## Release 部署与现场效率验收（2026-10-07）

本节记录最新 release 部署后的实际恢复状态，取代文中旧现场结论，以本节为准。旧数据保留为历史记录。§8.2 手工 UI 验收仍交用户执行，本轮未操作 UI。

执行 `cargo build --locked --release --features embedded-frontend` 一次，exit 0，48.00 秒。保留的 `target/release/usagi` 为 14,382,864 bytes（`du` 分配 14,048 KiB）。前端源码未改，没有重跑 `npm ci` 或前端 build。现场开始时 3210 无 listener、没有旧 Usagi 进程，原 LaunchAgent 未加载；使用原 plist bootstrap 后，新 release 由 PID 83729 持续运行并监听 `127.0.0.1:3210`，`/api/health` 返回 204。没有使用 CUA，也没有改 plist、签名或系统策略。

LaunchAgent 启动时将数据库中遗留的 running/followup 状态恢复为 idle/completed。随后只发出一次 `POST /api/refresh`，带 `X-Usagi-Request: 1`，HTTP 202、请求耗时 37.21 ms；扫描于 7.220 秒后 completed。扫描前 `data_revision=5271`，完成后为 5276。采样选取 year list 中一条实际有效的 Codex root，调用 Drawer 使用的 detail API；未保留 root ID。

每个读 API 共采样 3 次：前 2 次请求开始时 scan running，最后 1 次请求开始时 idle。表中为毫秒；所有请求均返回 HTTP 200，最大耗时 483.39 ms。

| API | Scan 样本（n=2） | Idle 样本（n=1） |
| :--- | :--- | :--- |
| Today summary | 21.13，97.22 | 15.49 |
| Year summary | 482.94，434.11 | 368.78 |
| Codex session list | 483.39，393.79 | 322.51 |
| Session rows | 20.38，20.60 | 14.61 |
| Codex Drawer detail API | 110.58，82.70 | 74.70 |

最终 idle 数据：today 2 个有效 roots、2 个 complete，总量 392,765,820 tokens；year 419 个有效 roots、419 个 complete，total sessions 为 424（另有 5 个 error roots），总量 14,159,997,241 tokens。实际 Drawer detail 返回 1 个 main model、49 个 subagents；Compaction tokens 分别为 main 1,888,289、subagents 合计 7,062,396。active Codex epoch/parser 为 7/12、build 为空；`scan_state=idle`、`active_scan_id=NULL`、followup 为空、最近 scan completed、`last_scan_error_code=NULL`。epoch 7 保留 5 个 quarantine roots，错误码均为 `USAGE_LEGACY_COVERAGE_AMBIGUOUS`，关联 87 条 quarantine source rows；usage checkpoints 为 1499 ready、87 rebuild_required。没有以 build manifest 清空推断 quarantine 已清除。

缓存清理前分配空间为：`target` 1,889,192 KiB、`frontend/node_modules` 247,104 KiB、`frontend/dist` 1,132 KiB、LaunchAgent stdout 4 KiB、stderr 0 KiB。清理后 `target` 为 14,048 KiB（仅保留 release binary）；`node_modules`、`dist`、`target/compaction-oversized-fix` 与两个 LaunchAgent raw log 均已删除。按清理前后 `du -sk` 实测，净回收 2,123,384 KiB。源码、正式 tests/docs/fixtures、用户数据库、`~/.codex` 和新 `target/release/usagi` 均保留。

raw log 清理后的唯一状态核验：原 LaunchAgent `state=running`、`pid=83729`，进程仍在；health HTTP 204，status HTTP 200，`scan_state=idle`、active scan 与 followup 均为空，最近扫描 completed，`last_scan_error_code=NULL`。该 status snapshot 的 `data_revision=5281`、`status_revision=15222`；它显示在已记录的手动 refresh 后又完成了一次 10.211 秒的 scan。期间没有第二个 refresh POST，未进一步调查该 scan 的触发来源。

## 历史记录：前一轮清理、构建与启动（2026-10-07）

按用户授权清理了旧 `target`、`frontend/node_modules`、`frontend/dist` 和旧 `/private/tmp/usagi-launchd.*` 输出；旧 `target/compaction-acceptance` 下所有 task raw logs 与 `STATUS.md` 一并删除。本文下方提及的这些日志路径仅保留历史文件名与结果，现已不可读取；历史结果继续保留，不为恢复日志而重跑。另清除了旧打包目录 `artifacts/.DS_Store` 及空目录，分配空间 8 KiB；正式 tracked 图标 `assets/windows/tray-icon.png` 保留。清理前 `du -sk` 分配空间为：`target` 12,483,460 KiB、`frontend/node_modules` 247,104 KiB、`frontend/dist` 1,132 KiB、旧服务输出 4 KiB、`artifacts` 8 KiB，合计 12,731,708 KiB。

`npm ci`、`npm run build`、`cargo build --locked --release --features embedded-frontend` 均退出 0。新 Cargo 输出曾占 463,044 KiB；清理中间文件后只保留 `target/release/usagi`（14,028 KiB 分配空间，14,363,680 bytes 文件长度）。本轮新建后又删除的 Cargo 中间产物为 449,016 KiB，`node_modules` 为 247,104 KiB，`frontend/dist` 为 1,132 KiB；合计 697,252 KiB。按上述路径计，最终相对清理前净减少 12,717,676 KiB；这是分配空间差额，不把临时构建占用重复计入。

复用了既有 LaunchAgent `com.hogeexxl.usagi.local`，其 plist 与目标路径均未修改。首次启动被旧 managed code signing 约束拒绝（`OS_REASON_CODESIGNING`）；`codesign --verify --verbose=4` 确认新二进制签名有效且为 ad-hoc linker signature，随后对同一 plist 执行 `bootout` / `bootstrap` 刷新 job 后启动成功，无需显式重签。PID 27512（PPID 1）执行 `target/release/usagi`，由该服务监听 `127.0.0.1:3210`，`GET /api/health` 返回 HTTP 204。当时服务 `running`。新运行 stdout 为 42 bytes、stderr 为 0 bytes，是此次启动产生的常规服务输出，不属于未清理的旧 raw logs。

没有新增 worktree。正式 schema、源码、fixtures、manifest、脚本及方案文档均保留；测试中的 `t_hf_re04_shadow_rebuild_cross_batch_none_to_single_completes` 失败分支 SQL 诊断已移除并恢复完成断言，epoch、usage 与 checkpoint 断言仍保留。§8.2 人工验收仍未执行；本次没有重跑测试或完整 Gate。

## 阶段状态

| 阶段 | 状态 | 证据 / 说明 |
| :--- | :--- | :--- |
| Phase 1 | 已执行，Gate 已验收 | 保留此前分阶段验收结论。原 /tmp 日志当前环境不可读；本记录不重建或补造日志。 |
| Phase 2 | 已执行，Gate 已验收 | 保留此前分阶段验收结论。原 /tmp/usagi-compaction-phase2-gate.log 当前环境不可读。 |
| Phase 3 | 已执行，Gate 已验收 | 保留此前分阶段验收结论。原 /tmp 定向日志当前环境不可读。 |
| Phase 4 | 已执行，Gate 已验收 | 保留此前分阶段验收结论：Rust 指定 case、前端 29 个测试及 npm run check 均已验收通过。原 /tmp 日志当前环境不可读。 |
| Phase 5 | 主协调已于 2026-10-07 按 §7.2 累计机器结果验收通过 | 完整脚本唯一一次历史运行退出 1，后续失败 case 均通过定向 exact 复验；没有第二次完整脚本运行，也没有声称完整脚本退出 0。§8.2 人工验收交用户，尚未执行。首次运行日志目录曾记录为 /tmp/usagi-compaction-phase5-20261007T050442Z-53414/，当前环境不可访问；首次结果来自此前 worker 会话报告。 |
| §8.2 人工验收 | 未执行 | 没有真实 UI 观察、浏览器操作或截图。 |
| GitHub 发布 | 未执行 | 本阶段没有发布 GitHub 版本。 |

### 证据来源与范围

本记录区分此前 worker 会话报告与当时可读的日志。首次完整 Gate、分阶段验收、单测修复、逐 target 首次执行、schema 定向复验、baseline 复现及 fixture 覆盖情况，依据此前 worker 会话报告和主协调验收；相应 `/tmp` 原始日志及 baseline 临时 checkout 当前不可访问。仓库内 `target/compaction-acceptance` 原始日志已按用户要求清理；没有把历史结果伪装成当前可读日志，也没有为恢复证据而重跑。

清理前仍可读的定向日志曾位于 `target/compaction-acceptance/continue-Y/`、`continue-X/` 与 `cost-four/`，对应的历史结果在下文列明，raw logs 现已删除。X 清理了 bulk case 的临时 phase、等待观察 helper 与大段数据库诊断；t_hf case 的 failure-branch SQL 诊断也已从测试中移除。此前带诊断与观察 helper 的进度日志只作为历史结果记录，原文件已清理。

此前一轮仅运行四个指定 exact case；当时 cost-four 日志保存最终通过输出，raw log 现已删除。三份受影响测试文件的 `rustfmt --check --edition 2024` exit 0；此前全量 `cargo fmt --check` 为 exit 0（历史文件名 continue-X/fmt-final.log）。此前 scoped `git diff --check` exit 0。没有重跑完整门禁。

### Phase 5 首次单一门禁

单一入口为 bash scripts/ci/check_codex_compaction.sh，首次且唯一一次运行退出 1。该脚本按次序收集 cargo check、cargo fmt --check、cargo test、前端 npm test、前端 npm run build 和范围限定的架构检查。首次运行的结果如下：

| 项目 | 实际命令 | 退出码 | 结果与证据状态 |
| :--- | :--- | :---: | :--- |
| Rust 类型与构建 | cargo check | 0 | 成功；输出包含既存 warnings。历史 worker 报告。 |
| 格式 | cargo fmt --check | 最终 0 | 完整门禁首次发现 src/codex/rollout.rs、src/codex/usage.rs、tests/track_d_antigravity.rs 格式差异，随后仅格式化这三个文件。最后 runtime 收口检查的首次结果见 continue-X/fmt-check.log（exit 1，5 hunks）；格式化本需求 pipeline 与 integration 测试路径后，continue-X/fmt-check-after-format.log 仍为 exit 1，仅剩 usage_processor.rs:5972/5982 两个格式 hunk。随后仅 rustfmt usage_processor.rs，最终 cargo fmt --check 在 continue-X/fmt-final.log 为 exit 0、0 hunk。 |
| Rust 全量测试 | cargo test | 101 | Cargo 在 lib test binary 失败后停止；lib 为 480 passed、22 failed、2 ignored。20 个 integration targets 与 usagi bin target 当时没有启动。后续逐 target 结果见下文。历史 worker 报告。 |
| 前端测试 | cd frontend && npm test | 0 | 29 个 test files、235 tests passed。历史 worker 报告。 |
| 前端构建 | cd frontend && npm run build | 0 | tsc --noEmit 与 Vite build 成功。历史 worker 报告。 |
| 架构残留检查，首次 | 脚本中的 architecture_residue_check | 1 | 首次误报合法 source_usage_epochs SQL 引用及 Rust DTO 逗号字段格式。历史 worker 报告。 |
| 架构残留检查，最小修复后定向执行 | 脚本中的 architecture_residue_check shell 函数 | 0 | 最小调整 checker 后定向检查通过；canonical schema 私有列和 DTO 检查仍保留。历史 worker 报告。 |
| 脚本语法，LOG_DIR 更新后 | bash -n scripts/ci/check_codex_compaction.sh | 0 | 当前语法检查输出 0 行、exit 0；日志为 continue-X/bash-n.log。 |

以上结果不构成整套 Gate 退出 0；唯一一次完整脚本仍为退出 1。

### lib 失败的定向修复与边界

首次 lib 结果为 480 passed、22 failed、2 ignored（共 504 个当时报告的测试结果；Cargo 退出 101）。两个 worker 分别处理 5 项与 17 项失败；据此前 worker 会话报告，22 个原失败 case 均已逐项修复并 exact 复验通过：

| 组 | 原失败数 | 修复与复验报告 |
| :--- | :---: | :--- |
| Processor / pipeline | 5 | 涉及 parser / pipeline 计数、六维 usage、effort、compensation 分类与合法 durable context fixture / patch 的断言；5 项逐项 exact 复验通过。 |
| Storage / rebuild / binding | 17 | 涉及 schema 13 到 14、parser 版本 12、rebuild durable context、fixture 与 patch 顺序等；17 项逐项 exact 复验通过。合法 fixture 与 patch 不依赖数组顺序。 |

此后新增了 1 个 occurrence explicit-delete 保护 case，并据报告 exact 通过。因此当前测试清单比首次 lib 清单多 1 项，共 505 项；这只是测试清单数量，不代表 505 项曾在同一轮中运行。storage occurrence guard 的最小实现确保不同物理身份不会静默覆盖；只有显式 delete 且目标未被 hold 时才允许 retarget。直接受该 guard 影响的 4 个 case 据报告均 exact 通过。以上定向 lib 结果来自此前 worker 报告；没有最终整 lib 重跑，也没有声称整 lib 已整体通过。

### 20 个 integration targets 与 bin 的逐项执行

首次完整 cargo test 停在 lib，因此当时未启动下列 20 个 integration targets 与 cargo test --bin usagi。之后每个 target 均被单独执行一次；表中保留首次逐 target 结果。已修复 case 的 exact 复验另列于表后，不将 target 全量重跑。

| 分组 | target | 实际命令 | 首次逐项结果 |
| :--- | :--- | :--- | :--- |
| A | codex_compaction_integration | cargo test --test codex_compaction_integration | 10 passed、1 failed，exit 101；失败 case 后由 Y exact 复验通过。 |
| A | spec01_phase2_ingestion | cargo test --test spec01_phase2_ingestion | 9 passed、0 failed，exit 0。 |
| A | spec01_phase3c_lifecycle | cargo test --test spec01_phase3c_lifecycle | 6 passed、0 failed，exit 0。 |
| A | spec03_scanner_integration | cargo test --test spec03_scanner_integration | 4 passed、0 failed，exit 0。 |
| A | spec04_mu04_integration | cargo test --test spec04_mu04_integration | 1 passed、2 failed，exit 101；两个失败均为 baseline 成本断言。 |
| A | spec04_usage_integration | cargo test --test spec04_usage_integration | 9 passed、5 failed，exit 101；2 个 schema 断言后来 exact 通过，1 个为 baseline 成本断言，另 2 个 runtime case 已有清洁代码 exact 通过证据。 |
| A | spec05_api_integration | cargo test --test spec05_api_integration | 11 passed、1 failed，exit 101；失败 case 后由 Y exact 复验通过。 |
| B | bin usagi | cargo test --bin usagi | 3 passed、0 failed，exit 0。 |
| B | distribution_build_guard | cargo test --test distribution_build_guard | 4 passed、0 failed，exit 0。 |
| B | distribution_ci_guard | cargo test --test distribution_ci_guard | 3 passed、0 failed，exit 0。 |
| B | distribution_launcher_integration | cargo test --test distribution_launcher_integration | 1 passed、0 failed，exit 0。 |
| B | distribution_public_repo_guard | cargo test --test distribution_public_repo_guard | 1 passed、0 failed，exit 0。 |
| B | distribution_release_guard | cargo test --test distribution_release_guard | 5 passed、0 failed，exit 0。 |
| B | distribution_runtime_integration | cargo test --test distribution_runtime_integration | 1 passed、0 failed，exit 0。 |
| B | replayed_ancestor_regression | cargo test --test replayed_ancestor_regression | 2 passed、0 failed，exit 0。 |
| B | spec01_phase5_source_filter | cargo test --test spec01_phase5_source_filter | 7 passed、0 failed，exit 0。 |
| B | spec02_metadata_integration | cargo test --test spec02_metadata_integration | 4 passed、0 failed，exit 0。 |
| B | spec05_api_stress | cargo test --test spec05_api_stress | 0 passed、0 failed、1 ignored；ignored case 未执行。 |
| B | spec06_frontend_browser | cargo test --test spec06_frontend_browser | 0 passed、0 failed、1 ignored；ignored case 未执行，不能算 UI / browser 验收。 |
| B | track_d_antigravity | cargo test --test track_d_antigravity | 10 passed、1 failed，exit 101；失败为 baseline 成本断言。 |
| B | usage_summary_public_surface | cargo test --test usage_summary_public_surface | 2 passed、0 failed，exit 0。 |

A 组首次逐 target 合计 50 passed、9 failed；B 组含 bin 合计 43 passed、1 failed、2 ignored。首次合计为 93 passed、10 failed、2 ignored。之后 6 个原失败 case（lifecycle 1、API 1、schema 2、runtime 2）已有 exact 通过日志；本轮四项成本 / catalog case 也已 exact 通过。更新后的 integration targets 与 bin 唯一 case 汇总为 103 passed、0 failed、2 ignored：103 个非 ignored case 有通过结果，另 2 个 ignored 未执行。此汇总仅指 integration targets 与 bin，不包括 lib；它不是一次完整 cargo test 的结果。定向复验可能重复执行同一 logical case，汇总始终按唯一 case 当前结果计算。

清理前读取的 Y 日志记录以下 exact 通过证据；表内文件名现已不可读取：

| exact case | 实际命令 | 结果 | 历史日志文件（已删除） |
| :--- | :--- | :--- | :--- |
| compaction_lifecycle_replay_snapshot_and_shadow_rebuild | cargo test --test codex_compaction_integration compaction_lifecycle_replay_snapshot_and_shadow_rebuild -- --exact --nocapture | exit 0；1 passed、0 failed、10 filtered；165.02 秒。无 timeout 参数变更。 | target/compaction-acceptance/continue-Y/lifecycle-exact.log |
| t_s05_021_concurrent_usage_queries_and_real_scan_never_expose_partial_snapshot | cargo test --test spec05_api_integration t_s05_021_concurrent_usage_queries_and_real_scan_never_expose_partial_snapshot -- --exact --nocapture | exit 0；1 passed、0 failed、11 filtered；0.82 秒。 | target/compaction-acceptance/continue-Y/api-exact.log |

API case 只接受 (snapshot_revision,total) 为 (r,30)、(r+1,30) 或 (r+2,35) 的三个合法快照；r+1 对应 null 到 0 分类激活，仍断言拒绝半写。通过该测试没有修改运行时 API 事务。

### 两个 runtime case：清洁代码 exact 通过

两个 runtime case 均已有清洁代码 exact 通过证据，各 1 passed、0 failed、13 filtered、exit 0。第二个 bulk case 的 wait_scan_for helper 使用两次 30 秒等待；其余 wait_scan 默认仍为 8 秒。临时 phase 诊断、eprintln 与大段数据库诊断已删除。此前含临时诊断 / 观察 helper 的 progress 日志结果只作为历史结果保留在本记录中，原始文件已清理，不作为清洁代码结果。

| exact case | 实际命令 | 结果 | 历史日志文件（已删除） |
| :--- | :--- | :--- | :--- |
| t_hf_re04_shadow_rebuild_cross_batch_none_to_single_completes | cargo test --test spec04_usage_integration t_hf_re04_shadow_rebuild_cross_batch_none_to_single_completes -- --exact --nocapture | exit 0；1 passed、0 failed、13 filtered；0.38 秒。 | target/compaction-acceptance/continue-X/t_hf_re04_pipeline_fix.log |
| t_s04_030_041_buildfrom_multibatch_and_localreplay_over_budget_promotes_to_shadow_build | cargo test --test spec04_usage_integration t_s04_030_041_buildfrom_multibatch_and_localreplay_over_budget_promotes_to_shadow_build -- --exact --nocapture | 清洁代码 exact：exit 0；1 passed、0 failed、13 filtered；30.41 秒。 | target/compaction-acceptance/continue-X/t_s04_030_041_final_30s_exact.log |

此次 runtime 修复的生产逻辑位于 usage_pipeline 的 process_local_replay：构造 Commit 前，如果 effective_tail 未 exhausted 或 status 为 Unverified，则转为 NeedsRebuild。storage 错误分类的尝试已恢复 InvalidState，没有通过改写错误分类绕过失败。bulk case 的临时等待观察诊断已从清洁版本移除；t_hf case 的早期 failure-branch SQL 诊断现已移除。另一份进度观察日志曾为 1 passed、27.87 秒并包含 bulk case 临时诊断；清洁版本结果以历史记录中的 final_30s_exact 结果为准。相关 raw logs 已按用户要求删除。

### 四项 baseline 成本断言

此前 worker 报告称，以下 4 项在隔离 baseline commit 72a4334616972da90fdd612e928a9437e27521c0 上均以同一实际失败精确复现。baseline 临时 checkout 与原始日志当前不可访问；以下内容保留自此前已验收的 worker 报告，不表示本 worker 重新复现：

| 测试 | baseline 实际值与断言 |
| :--- | :--- |
| track_d_antigravity::test_td_p3_import_01_complete_standalone_fixture | 首条 Flash 事件实际 Some(2351550)，旧循环断言 None；fixture 计算为 (1318*750 + 24*75 + 363*3750)。 |
| spec04_mu04_integration::t_mu04_f02_parser4_pricing1_reprice_and_shadow_rebuild_stay_independent | catalog 实际为 7，旧断言为 4。 |
| spec04_mu04_integration::t_mu04_f03_reserve_historical_reprice_preserves_identity_and_epoch | catalog 实际为 7，旧断言为 4。 |
| spec04_usage_integration::t_mu03_s02_version_upgrades_remain_independent | 实际 (1,7)，旧断言为 (1,4)。 |

2026-10-07，用户明确授权仅更新这 4 个旧测试期望。本轮没有改 catalog、计费 runtime、fixtures、manifest、hash 或其它断言。track_d 的 standalone 数据库按 occurred_at 排序为 Flash、Pro、Flash：Flash 两项分别精确期望 Some(2,351,550) 与 Some(3,382,500)，Pro 在现有 catalog 中无费率，保留 None；对应公式为 1318*750 + 24*75 + 363*3750，以及 2000*750 + 100*75 + 500*3750。两个 mu04 测试的 catalog 版本期望及 mu03 同一测试内三处 catalog 版本期望由 4 更新为 7，cost algorithm 仍为 1，其它 parser / epoch / metadata 断言未改。

| 最终 exact case | 实际命令 | 最终结果 | 历史日志文件（已删除） |
| :--- | :--- | :--- | :--- |
| track_d_antigravity::test_td_p3_import_01_complete_standalone_fixture | cargo test --test track_d_antigravity test_td_p3_import_01_complete_standalone_fixture -- --exact --nocapture | exit 0；1 passed、0 failed、10 filtered。 | target/compaction-acceptance/cost-four/track-d-antigravity.log |
| spec04_mu04_integration::t_mu04_f02_parser4_pricing1_reprice_and_shadow_rebuild_stay_independent | cargo test --test spec04_mu04_integration t_mu04_f02_parser4_pricing1_reprice_and_shadow_rebuild_stay_independent -- --exact --nocapture | exit 0；1 passed、0 failed、2 filtered。 | target/compaction-acceptance/cost-four/mu04-f02.log |
| spec04_mu04_integration::t_mu04_f03_reserve_historical_reprice_preserves_identity_and_epoch | cargo test --test spec04_mu04_integration t_mu04_f03_reserve_historical_reprice_preserves_identity_and_epoch -- --exact --nocapture | exit 0；1 passed、0 failed、2 filtered。 | target/compaction-acceptance/cost-four/mu04-f03.log |
| spec04_usage_integration::t_mu03_s02_version_upgrades_remain_independent | cargo test --test spec04_usage_integration t_mu03_s02_version_upgrades_remain_independent -- --exact --nocapture | 最终执行 exit 0；1 passed、0 failed、13 filtered。 | target/compaction-acceptance/cost-four/mu03-s02.log |

mu03 exact 在全部旧 catalog 期望更新前曾停在 cost_only 的 (1,4)；随后 initial、cost_only、usage_only 三处 catalog 期望均改为 (1,7)，最终 exact 通过。主协调已于 2026-10-07 按实施方案 §7.2 接受累计机器结果。首次脚本 exit 1 保留为历史执行结果，没有第二次完整脚本运行，也不表示有新的完整脚本 exit 0。§8.2 人工验收交用户，尚未执行；两个 runtime case 均已有清洁代码 exact 通过记录。

## 历史升级验证边界

compaction_upgrade_readiness 使用 v6 计量 payload，在临时测试数据库中构造 parser v11 / canonical algorithm v5 的旧 metadata header，再经扫描与 activation 验证。它不是真实 v5 旧账本，也不能证明真实 v5 usage ledger 的升级过程。

## §8.2 人工验收步骤与 fixture 对应

下表保留实施方案 §8.2 的人工操作、观察标准与自动 fixture 对照。它是待人工执行的步骤清单，不是已完成人工验收的记录。真实脱敏 fixture 的自动集成结果不能替代真实 UI 观察；目前没有人工截图或浏览器验证。

| 操作 | fixture 与测试对应 | 观察标准 / 当前限制 |
| :--- | :--- | :--- |
| 打开单次 Compaction 的 Main 模型块 | modern_one.jsonl，compaction_modern_one | 在 Estimated Cost 上方显示 316,989；Total Tokens 是 6,129,586，已包含该 response。Compaction 行是分类子集，不再增加 Total Tokens 或 cost。尚无人工作观察。 |
| 打开有两次 Compaction 的 Main 模型块 | modern_two.jsonl，compaction_modern_two | 同一 gpt-5.6-sol / medium 块合计显示 506,360；Total Tokens 仍按实际 response 计算一次。它不是多模型 / 多 effort fixture。尚无人工作观察。 |
| 打开已扫描且没有 marker 的模型块 | normal_only.jsonl，compaction_normal_only | Compaction 显示 0。尚无人工作观察。 |
| 打开未知 Compaction 范围的 legacy 样本 | legacy_only.jsonl，compaction_legacy_only_unknown_marker | 有一个未解析 marker，Compaction 显示 —，不能把未知当作零。尚无人工作观察。 |
| 展开真实 Subagent 的模型块 | subagent.jsonl，compaction_detail_scope_real_subagent_usage_belongs_to_owning_child | child 自身显示 240,412；Root inclusive Total Tokens 为 24,951,138，不会因展示分类值而再次增加。fixture 有真实 child 样本和 parent metadata stub；尚无人工作观察。 |
| 查看 A/B/C root 与两个 child 的归属 | 仓库没有完整 A/B/C 外部 JSONL fixture；src/codex/analytics.rs 的 compaction_detail_scope 使用构造行 | 单线程 subagent.jsonl 不能代替 A/B/C。可按 §8.2 使用现有真实 A/B/C session；若无可用样本，记录为未覆盖，不以构造测试冒充人工结果。尚未执行。 |
| 查看 parser 11 active / parser 12 building 到 activation | compaction_upgrade_readiness；在 integration harness 中以 normal_only.jsonl 与 modern_one.jsonl 构造升级状态 | 仓库没有独立的人工 epoch fixture；自动测试不能替代真实升级过程中的人工观察。尚未执行。 |
| 扫描补齐 marker 后刷新详情 | compaction_late_evidence 等构造单测覆盖逻辑 | 仓库没有对应的完整外部 JSONL fixture；可按 §8.2 在有待解析 marker 的真实 session 上观察，未遇到时记录未执行。尚未执行。 |
| 打开其他 provider 的模型块 | 本需求没有专用非 Codex fixture | 按 §8.2 使用可用的真实非 Codex session 确认不出现 Compaction 行；无样本时记录未执行。尚未执行。 |

### 已有脱敏 fixture 的 SHA-256

以下 SHA-256 原样保留自 tests/fixtures/codex/compaction/manifest.json；本次没有修改 fixture、manifest 或 hash，也没有重算 hash。此前 worker 验收报告确认，逐项执行的 codex_compaction_integration 案例覆盖 8 个真实脱敏 fixture 的 manifest totals，以及 schema 14 四表、五索引、六 trigger、FK 零违规和 manifest 六维 actual / identity / ownership / hold / carry 等集成断言。该 target 首轮 10 项通过，唯一失败的 lifecycle case 后经 Y exact 复验通过。API target 的各项查询断言也有逐项执行结果；并非当时未启动。

| Fixture | SHA-256 | 用途 |
| :--- | :--- | :--- |
| normal_only.jsonl | 2c79f5495ca44310cbb4f4a2ad7bc008e4b48ba0559875195e941dbcad7c571e | 无 marker；零值展示 |
| modern_one.jsonl | affa01075bd43df263693b89224f077339e62548265b3678c4a4303e2f874ec0 | 单次 Compaction；316,989 |
| modern_two.jsonl | 3808ca728b36a0de0128dfe20ada04d1523f769ba5474e0240fa51baa1194d45 | 两次 Compaction；合计 506,360 |
| legacy_only.jsonl | 2b3ecce6c25b921f690e3da98a3cc451e66f9a9efd7dcd16b42edf1af04ff8fb | legacy unknown marker；显示 — |
| reset.jsonl | 2d2b8b17ce733736a7afb754cfd6634582d9afbcc638f009316fe3138145b71e | counter reset；三次 Compaction |
| resume.jsonl | 808e9d4dce651589cd3f1d2b2d49beff3f0f08e9b4e84fd34e5b64b89c766582 | 排除 inherited baseline |
| fork.jsonl | 2a822466c86e72c362714c47e574523fb030a223cac019f9086d6c7d4e807da8 | 排除 ancestor 历史 |
| subagent.jsonl | ef25af09b3736b48e232171210559d431075ded60a3ea5dc1add993931fb0d81 | 真实 Subagent owning child 用量 |
| schema_top_and_embedded.jsonl | 6a91c327eb99e53574ac119d1d99a729a72aeda1867daf3dfbd9b9e5de8a6559 | 顶层与内嵌 usage 同 response |
| schema_id_only.jsonl | 1b4911722ef3e781703dfbabf4ad97f1d23122396f629d9df7c7923da216349f | 只有 Compaction ID 的真实片段 |
| schema_usage_null.jsonl | 24e9e6b858d622a51ef1556009dcbbd6ad2d9ca1e13e39d4c0265f9afeaef12a | 从真实片段构造的 usage-null 解析样例 |
| schema_top_only.jsonl | 86b69ebe096475730f67bb1883c4be92cd3e2d3b4b07deaf96c66072a63b607e | 从真实片段裁剪的顶层 usage |
| schema_embedded_only.jsonl | c7aa9e78b2331cb61974119b2eb29b876e49520e3127f9bf11a5db9e249b069b | 从真实片段裁剪的内嵌 usage |
| schema_zero_context_estimate.jsonl | 2bd5ee7a3cbad800efba171d3d862cb9784f9fa19995e88cda36aa0896437bf2 | legacy 压缩后上下文估计，不是单次 usage |

schema 片段只用于解析 / 重放，不代表完整 session 总量。构造片段保留其构造标记，不作为真实 session 账目对外展示。

## 当前收尾状态

完整脚本只运行一次，exit 1 是保留的历史结果。按 §7.2，后续针对失败项的定向 exact 复验已通过；主协调已于 2026-10-07 接受累计机器结果。首次 lib 的 22 个失败与新增 occurrence guard case 有定向 exact 通过报告，但没有最终整 lib 运行。20 个 integration targets 与 bin 已各自执行；按当时汇总为 103 passed、0 failed、2 ignored，其中两个默认 ignored 未执行。四项 baseline 成本 / catalog case 与此前两个 runtime case 均有 exact 通过历史记录；对应 raw logs 已按用户要求删除。原最终 cargo fmt --check 为 exit 0（历史文件名 continue-X/fmt-final.log）；此前 scoped rustfmt 与 scoped git diff 检查均 exit 0。此前脚本 `bash -n` 为 exit 0（历史文件名 continue-X/bash-n.log）。没有第二次完整脚本运行，也没有声称完整脚本新 exit 0。§8.2 人工验收交用户，尚未执行，是当前唯一尚未完成项。

§8.2 人工验收全部未执行，没有真实 session 的 UI 观察、截图或浏览器验证。A/B/C 无完整外部 JSONL fixture，Subagent fixture 有真实 child 和 parent metadata stub；升级 seed 为 v6 payload 加 v11 / v5 header，并非真实 v5 ledger。  

### 2026-10-07 现场回归补记：event occurrence 查询与未解决的 oversized 冲突

本补记更新上文“当前收尾状态”：前述 Phase 5 累计机器验收不代表本次真实运行故障已恢复。用户报告今天的 Codex 会话全无、网页和 Drawer 加载慢、同步不结束。正式服务此前有 Codex build epoch 7、parser 12，active epoch 6、parser 11；正式库只读汇总显示 1492 个 rebuilt、3 个 pending、87 个 quarantined。active 6 未恢复成有效 Codex 账目；部署后 Codex today/year API 仍为零，status scan 报 SOURCE_RUN_FAILED / USAGE_GROUP_COMMIT_FAILED。没有清除 quarantine 或手工激活 build epoch。

现场调用栈集中在 `load_event_occurrences()` 的 SQLite step。生产查询默认计划按 epoch 扫描，单个 occurrence 查询耗时 17.088 ms；强制使用既有 `codex_usage_event_occurrences_event_idx` 后同一行耗时 0.087 ms，约 196 倍差异。只在 occurrence loader 查询加了现有索引提示，没有新增 schema、migration 或索引。以下 4 个定向 case 均各有 1 passed、0 failed 的实际结果；3 个库 case 使用完整模块路径和 `--exact`，integration 使用单测试名 filter（未带 `-- --exact`），实际 1 passed、13 filtered、25.55 秒：

| 直接回归 | 结果 |
| :--- | :--- |
| `compaction_event_occurrence_lookup_is_sparse_with_unrelated_rows` | 验证稀疏 event 查找、结果和排序 |
| `compaction_closed_turn_late_rewrite` | 验证受影响 reconciliation rewrite |
| `compaction_replay_orphan_hold` | 验证 replay orphan hold |
| `t_s04_030_041_buildfrom_multibatch_and_localreplay_over_budget_promotes_to_shadow_build` | 验证多批次 build 与 over-budget local replay；单测试名 filter，1 passed、13 filtered、25.55 秒 |

此索引修复已随 release 构建部署。首次 kickstart 后进程停在 dyld open；对原 LaunchAgent plist 执行 bootout/bootstrap 后，PID 38371 成为运行进程并监听 localhost:3210，正式数据库已打开。未修改 plist、签名或安全策略。健康端点返回 204，但 status / Codex 数据验收失败，不能据此算现场恢复。

在只读正式库的一致临时 DB 副本上，沿现有 `run_usage_round()` 用首个 pending group 和原 JSONL 路径复现。只有 `source_file_id=1098` 有直接复现证据：BuildFrom、replay prefix 0、adapter bytes 9,959,482、1 行、canonical 0、occurrence 0、evidence/write units 1，唯一 patch 是一个既有 open Turn upsert。底层错误为 `InternalStorageError("usage batch exceeds fixed budget")`。该 source 的正式 checkpoint 为 parser 12、offset 28,470,721、ready；build epoch 7 的 required offset 也是 28,470,721、状态 pending。其余两个 pending source 未分别隔离复现，不能据此宣称三项均由同一情形阻塞。

核对发现 `Gap::Oversized` 会更新 `chain_state`，并把既有 `open_turn.blocks.parser_gap` 设为 true；`finish()` 的 Turn snapshot 写入承载该 block 和 partial quality。storage 当前允许单条超过 8 MiB 的独占进度提交，但要求 write units 为 0 且无 Turn upsert。省略 upsert 会使后续 `read_open_turn()` 从旧 `codex_turns` 行恢复出未阻断的 Turn；若该行是 EOF，build 可在同一批完成，之后没有保证的记录触发补写。因此不能通过清空 patch 或延迟到“下一行”解决。

当时发现 §4.3.5 oversized-only 零写入规则与必须持久化的 Gap Turn evidence 冲突；用户随后批准了单条 Turn upsert 例外。已在 source / group 两个预算判断共用纯内存 predicate：保持普通 byte / line 规则；0 Turn upsert 沿用独占零写入进度（包括 ReplayedAncestor 和既有 anomaly）；唯一 Turn upsert 分支要求当前 source generation 的既有 open Turn、Interrupted(Oversized)、parser gap、partial quality 与提交后的 state offset 一致，并禁止计量 patch、fact、marker、window、hold、Turn rewrite 和 delete。没有新增 SQL、schema、finish 入口或其它生产路径。§8.2 人工验收仍未执行。

本轮收尾清理已完成：删除 `target/compaction-regression` 临时 DB / sample / debug 产物、`target` 下 release 二进制以外的构建中间产物，以及 `frontend/node_modules` 和 `frontend/dist`；保留 `target/release/usagi`。清理前 `target` 为 3,099,472 KB，清理后仅保留的二进制为 14,028 KB；前端两项目录分别为 247,104 KB 和 1,132 KB。按 `du -sk` 分配空间差值，本轮释放 3,333,680 KB（3,413,688,320 字节，约 3.18 GiB）。没有重跑测试、构建或启动服务。

### 2026-10-07 oversized budget 修复定向复验

仅运行以下两个 exact 用例，均实际 `running 1`、`1 passed`、exit 0；Cargo 日志位于 `target/compaction-oversized-fix/`：

| Exact case | 结果 |
| :--- | :--- |
| `codex::storage::usage::tests::tests::oversized_gap_turn_commit_completes_build_and_rejects_mixed_batches` | EOF offset fixture 写入 Interrupted(Oversized) source state、partial parser-gap open Turn 与对应 checkpoint；build source 标记 `rebuilt`。同时拒绝计量 occurrence/event 混入、marker 混入和多 source 合批；ReplayedAncestor 零 write-unit 进度及 anomaly 保持可接受。1 passed、0 failed、506 filtered。 |
| `codex::ingestion::usage_pipeline::tests::exclusive_large_and_oversized_batches_preserve_contract_without_fake_candidates` | 保留无 open Turn 的零写入用例，并验证真实 pipeline DTO 对已有 open Turn 输出 1 个 partial parser-gap Turn upsert，canonical / occurrence 为零。1 passed、0 failed、506 filtered。 |

首次存储用例命令因遗漏一层 `tests` 路径而 `running 0 tests`，不计通过；随后用表中完整路径重新执行并通过。首次 fixture 初始化也因未包含全部 present source 被 rebuild utility 拒绝，已按其 discovery proof 要求修正 source 列表并通过。此次没有运行整套 Phase 3 / 全 `cargo test` / npm tests，也没有构建或部署。

### 2026-10-07 oversized 修复构建与现场部署复验（未通过）

按用户授权各执行一次构建：前端 `npm ci` exit 0，`npm run build` exit 0（Vite 2.16 秒）；根目录 `cargo build --locked --release --features embedded-frontend` exit 0（release profile 1 分 10 秒，编译器报告 56 条 warning）。日志位于 `target/compaction-oversized-fix/`。新二进制路径仍为 `/Users/hogee/Desktop/Usagi/target/release/usagi`，SHA-256 为 `63b7099cf5dd9da7be136bf8fd58a3ce48077ffafabdd84214fcac2ca019b563`。

对既有 `com.hogeexxl.usagi.local` LaunchAgent 使用原 plist 执行 bootout/bootstrap，没有改 plist、签名或安全策略。初次加载 PID 63194 持续停在 dyld `_open`，等待超过 3 分钟仍未监听。按用户随后授权的 inode 刷新流程再次 bootout，确认 PID 63194 已退出；将同一二进制复制到同目录临时文件，复制前后 SHA-256 一致、`codesign --verify --verbose=2` 均显示 `valid on disk` 和 `satisfies its Designated Requirement`，随后原子替换。inode 从 `1433640811` 变为 `1433650018`，签名验证再次通过，再以原 plist bootstrap 得 PID 64341。

PID 64341 启动约 2 分 3 秒时的短 sample 仍停在 `dyld4::Loader::getOnDiskBinarySliceOffset` → `mapFileReadOnly` → `open`；约 3 分 27 秒时 TCP 3210 仍未监听。`http://127.0.0.1:3210/api/health` 连接失败（HTTP 000），没有发送 `/api/refresh`。`spctl --assess --type execute --verbose=4 target/release/usagi` 返回 exit 3、`rejected`；代码签名信息显示为 `adhoc,linker-signed`、无 TeamIdentifier。最近 5 分钟仅筛选 usagi 及相关 syspolicyd/launchd 拒绝或延迟信息的系统日志无匹配记录；`/tmp/usagi-launchd.out` 最后修改时间仍为 18:07:58，`/tmp/usagi-launchd.err` 最后修改时间仍为 17:32:20 且大小为 0。现有证据确认系统执行评估为 rejected，但命令和相关日志没有给出更具体的拒绝原因；未移除 provenance/xattr、未调用 sudo 或修改系统策略。

生产库以 SQLite readonly 方式核对：Codex active epoch/parser 仍为 6/11，build epoch/parser 为 7/12；build 7 当前 `rebuilt=1495`、`pending=3`、`quarantined=87`，87 项仍为 `USAGE_LEGACY_COVERAGE_AMBIGUOUS`。三个 pending source 为 1098、1197、1198，checkpoint 均 ready 且已到 required offset；source 1098 的 offset 为 28,470,721。`app_meta` 当前 `scan_state=failed`、`active_scan_id=NULL`、followup 字段均为 NULL、`last_scan_error_code=SOURCE_RUN_FAILED`；最近的 scan rows 也为 `SOURCE_RUN_FAILED`。服务未进入 API，故 today/year/list/session-rows 和真实 Codex Drawer timings 均未取得，不能记录为恢复通过；也不能确认 build 7 已自然激活为 active 7。

本次失败诊断阶段按用户最新指示暂未清理。当前 `du -sk`：`frontend/node_modules` 247,104 KB、`frontend/dist` 1,132 KB、`target/debug` 1,365,384 KB、`target/release` 463,788 KB（其中 `target/release/usagi` 14,032 KB）、`target/compaction-oversized-fix` 80 KB。日志、sample、debug 与其它 release 中间产物仍保留，待用户判断后再处理。
