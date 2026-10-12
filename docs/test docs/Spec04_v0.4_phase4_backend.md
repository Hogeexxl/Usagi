# Spec04 v0.4 Phase 4 Backend Gate Record

Scope: §6.8 final backend Go/Rust validation and failure diagnosis. This record reflects the commands actually run in this workspace. It does not claim the Rust `--all-targets` gate completed successfully.

## Focused Go gates

Command:

```text
go test ./internal/api ./internal/api/testsupport ./internal/magpie ./internal/query ./internal/projection
```

Result: exit code 0.

```text
ok github.com/Hogeexxl/Usagi/internal/api 15.297s
ok github.com/Hogeexxl/Usagi/internal/api/testsupport 0.467s
ok github.com/Hogeexxl/Usagi/internal/magpie (cached)
ok github.com/Hogeexxl/Usagi/internal/query (cached)
ok github.com/Hogeexxl/Usagi/internal/projection (cached)
```

Command:

```text
go vet ./internal/api ./internal/api/testsupport ./internal/magpie ./internal/query ./internal/projection
```

Result: exit code 0; no diagnostics.

The user explicitly waived the baseline and final whole-repository Go Gates, stating: “接受记录该失败，豁免基线和最终全仓库 Go Gate”. The stated reason is the pre-existing compiler failure in `internal/codex/scan.go` at baseline HEAD `bca8c3dd9e55b0360d78dce3434690dc7d87a2b6`. This phase did not run `go test ./...` or `go vet ./...`, did not change the scanner, and does not claim either whole-repository gate passed.

## Rust `--all-targets` gate

Command actually started:

```text
CARGO_BUILD_JOBS=2 cargo test --all-targets
```

Observed output before the integration binary was stopped:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 1m 30s
test result: ok. 505 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 121.95s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.19s
test compaction_upgrade_readiness ... FAILED
```

The first two summaries are respectively the library test target and `src/main.rs`. In `tests/codex_compaction_integration.rs`, nine other test names had printed `... ok`; another test, `compaction_lifecycle_replay_snapshot_and_shadow_rebuild`, was still running. The integration test binary had not printed its final summary or the captured panic detail when the run was interrupted.

After the failure was reported, the still-running failed Gate process was interrupted with Ctrl-C so it would not remain running. The session returned `exit_code=130`; this is the observed interruption status, not Cargo's natural exit status for a completed test suite. The captured stream identified `compaction_upgrade_readiness` as failed but omitted the panic detail. Root then authorized one focused diagnostic run within this existing Gate; the result is recorded below. This does not change the status of the incomplete `--all-targets` command.

### Authorized focused failure diagnosis

Command:

```text
CARGO_BUILD_JOBS=2 cargo test --test codex_compaction_integration compaction_upgrade_readiness -- --exact --nocapture
```

Complete stdout/stderr was retained at `/tmp/Usagi_Spec04_phase4_rust_failure.log`. The command exited naturally with code 101 and reported:

```text
thread 'compaction_upgrade_readiness' panicked at tests/codex_compaction_integration.rs:1655:5:
assertion `left == right` failed: shadow build does not replace the seeded parser-11 active epoch
  left: 2
 right: 1
test compaction_upgrade_readiness ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 10 filtered out; finished in 0.21s
error: test failed, to rerun pass `--test codex_compaction_integration`
```

At line 1655, `left` is the current `harness.active_epoch().0` and `right` is the expected epoch passed to `assert_active_epoch_stays_visible`. The caller captures `old_active_epoch` before the parser upgrade at line 2282. The failing helper call is at line 637: it occurs immediately after `seed_parser11_algorithm5_history` and before the manual scanner request at line 643. Therefore, the assertion observed active epoch 2 where the seeded parser-11 epoch was expected to remain epoch 1; the test had not yet started the parser-upgrade scan.

Process check after interruption:

```text
ps -Ao pid,ppid,stat,etime,args | rg 'cargo test --all-targets|codex_compaction_integration-9812e48069dd9715'
```

Result: no Cargo/test process remained; the only matching rows were the `zsh` command running the check and its `rg` process.

### User-authorized final Rust Gate waiver

The user authorized: “记录并豁免本次最终 Rust Gate，保持 Spec04 范围，完成本地提交并交我验收”. Accordingly, the final Rust Gate is recorded and waived. The observed failures above remain failures: this waiver does not convert either Rust command into a passing result and does not establish a baseline cause. This is separate from the user's existing whole-repository Go Gate exemption. The other machine Gates required for this Spec04 handoff passed; user-facing manual acceptance remains pending.

## Failure path and relationship to Spec04 changes

Facts from the unchanged Rust test source:

- `tests/codex_compaction_integration.rs:2258` defines `compaction_upgrade_readiness`.
- The test creates an isolated `CompactionHarness`, installs embedded fixture bytes into a temporary Codex home, starts an in-process `IngestionCoordinator`, reads a session detail snapshot through the Rust `UsageLedger`, seeds parser-11 history, activates parser-12 data, and checks the resulting detail, usage, and data revision.
- The reproduced failure occurs in the test's Rust helper `assert_active_epoch_stays_visible`, while checking the Rust `Ledger` after seeding parser-11 history. The test path includes the Rust ingestion/storage/usage code and `src/codex/analytics.rs`; it does not call `internal/api`, `internal/query`, `internal/magpie`, or load `frontend/dist`.
- At diagnosis time, `git status` and `git diff --name-only` over Rust source, `Cargo.toml`, `Cargo.lock`, `src/windows_shell.rs`, and `src/platform/**` returned no changed paths. The actual Spec04 worktree changes are in Go and frontend paths.

Inference: the reproduced assertion executes through a self-contained Rust fixture/SQLite/in-process path, with no direct runtime dependency on the changed Go API/query code or frontend application. A direct execution-path cause from those changes is unlikely. The available evidence does not establish whether the Rust failure predates Spec04 or identify why the active epoch is already 2 at this assertion; no baseline attribution is made.

No Rust, Cargo manifest/lock, native Windows tray, or process bootstrap file was modified for this phase. No tests were run beyond the requested `--all-targets` Gate and the single authorized failing-test diagnostic command above. No code or dependency was changed during diagnosis.
