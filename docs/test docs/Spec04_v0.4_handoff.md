# Spec04 v0.4 → Spec05 Handoff

Spec04 freezes the data, HTTP, revision, watcher, and Dashboard contracts. Spec05 owns production startup, lifecycle, and packaging. This handoff describes the current seams and records seven future responsibility groups; it does not implement any of them.

## Current Go HTTP server seams

- `api.NewServer(config, dependencies)` builds the handler from `Projection`, `Query`, `Revisions`, `RuntimeStatus`, and `QuotaSnapshot`. It accepts Runtime in `starting`, defaults to `127.0.0.1:3210`, validates loopback listen addresses, and exposes the configured address through `ListenAddress()`.
- `Server.Start(ctx)` is idempotent. It starts the Runtime-to-revision subscription and the three configured Usage, Session, and Quota watchers. It does not start or own `magpie.Runtime`.
- `Server.HTTPServer()` returns `*http.Server{Addr: config.ListenAddress, Handler: server}`. Spec05 can own this object and its `Shutdown(ctx)` lifecycle.
- `Server.ListenAndServe()` is a convenience path: it creates a background cancellation context, calls `Server.Start(ctx)`, then calls `HTTPServer().ListenAndServe()`. It does not start `magpie.Runtime`, install process signal handling, or perform an explicit graceful HTTP shutdown. Spec05 must choose and wire the process-level lifecycle.
- `api.Config.StaticFS` is the filesystem injection seam. Tests supply `fstest.MapFS`; when no `StaticFS` is supplied the server uses `StaticDir`, whose default is `frontend/dist`. Production embedded assets are not connected here; there is no production `embed.FS` handoff in Spec04.
- The repository's current Go `main` is `cmd/storage-check/main.go`, which operates on the storage utility. No product Go entry point constructs the Runtime → Query → Projection → API graph yet.

The service graph inputs already exist. `magpie.Runtime` implements `query.ClientProvider` through `Client()`, so `query.NewRuntimeService(runtime)` looks up the current client dynamically; `projection.NewService(queryService)` consumes that Query service. The API dependencies can be assembled as:

```go
runtime := magpie.NewRuntime()
queryService := query.NewRuntimeService(runtime)
projectionService := projection.NewService(queryService)

server, err := api.NewServer(api.DefaultConfig(), api.Dependencies{
	Projection:    projectionService,
	Query:         queryService,
	Revisions:     api.NewRevisionService(),
	RuntimeStatus: runtime,
	QuotaSnapshot: &api.QuotaSnapshot{},
})
```

Spec05 owns sequencing `runtime.Start(ctx)`, server watcher startup, listener startup, cancellation, `runtime.Close()`, and HTTP shutdown for the actual product process. The example describes existing constructors and interfaces; it is not a committed bootstrap or production startup recommendation beyond those seams.

## Seven Spec05 responsibility groups

1. **Formal `main` and bootstrap wiring.** Construct the Runtime → Query → Projection → API dependency graph and integrate it with the app entry point and app lifecycle.
2. **Embedded static assets in production.** Connect the React build output to `Config.StaticFS` and the packaged application. The existing filesystem seam is ready; the production embedded source is not wired.
3. **Port 3210 ownership.** Coordinate the default loopback listener with the desktop app, handle port conflicts and ownership, and preserve the localhost-only API boundary.
4. **Shutdown and macOS app lifecycle.** Own cancellation, graceful HTTP shutdown, Runtime process close, and the app's launch/quit behavior. The current `ListenAndServe()` helper does not provide this orchestration.
5. **Windows tray-to-Go backend bridge.** Start, monitor, and stop the Go service from the Windows native tray/bootstrap layer. Spec04 did not change that native startup chain.
6. **Rust API/backend retirement.** Retire the Rust service after the Go service is wired and accepted. Rust source remains present at this handoff; Spec04 did not remove it.
7. **CI, build/release packaging, and final smoke/cutover.** Produce platform artifacts with the embedded frontend and Go service, run packaged-app smoke checks, then make the product cutover. Spec04 does not claim a production switch or release.

Spec05 must preserve the frozen Spec04 Usage semantics, dynamic Agent and Provider/Model filters, ThreadKey/session hierarchy, Public v1 paths and DTOs, 30/30/60 watcher intervals, cached Quota behavior, and Revision/SSE/epoch-reset contract. Those contracts are documented in the [Spec04 execution record](Spec04_v0.4_execution.md) and phase Gate records.

## Acceptance checklist and current limitations

### Current code and contract acceptance

The current code and Public contract can be reviewed against the Phase 0–4 Gate records, §12 evidence in the [execution record](Spec04_v0.4_execution.md), and the Phase 2 decode of 14 actual Go handler response bodies using the existing PeekFlow Swift DTOs. This verifies code and wire-contract evidence; it does not mean a packaged Dashboard or PeekFlow app is connected to the Go server. There is no formal product Go bootstrap yet.

### Windows timezone test available now

On a Windows host, with `TZ` absent, run this existing test from the repository root:

```powershell
go test -count=1 ./internal/api -run '^TestSystemTimezoneUsesOSDefaultWhenTZIsUnset$'
```

The test unsets `TZ` itself and verifies that Windows returns a valid IANA timezone. Spec04 cross-compilation on macOS does not exercise WinRT at runtime; this Windows test remains unverified on a Windows host.

### Real Dashboard/PeekFlow UI acceptance after Spec05 wiring

These checks depend on Spec05 connecting the Go server, embedded frontend, and desktop app startup. Run them with the actual packaged product after that wiring is accepted:

- Launch the real Dashboard and PeekFlow and confirm their requests and displayed values against the running service.
- Observe Runtime startup from `starting` to `ready`; restart Go/Magpie while Summary, Session Table, or Session Detail is open, then confirm the new epoch reload and usable Detail.
- Verify cached Quota updates and the manual refresh action in the actual UI.
- Inspect real main/subagent Session hierarchy, inclusive totals, full `ThreadKey` detail requests, and returned children.
- Verify dynamic Agent and Provider choices, including distinct selection when the same model is available from multiple providers.

Final related Go package tests/vet and frontend tests/build/search passed; the whole-repository Go Gate retains the user's explicit exemption. Rust `cargo test --all-targets` reported `compaction_upgrade_readiness` failed an active-epoch assertion with actual `2`, expected `1`, after parser-11 history seeding and before the manual scanner request; the full run was then interrupted with Ctrl-C/exit 130. The focused command exited naturally with 101. The cause is not attributed to baseline. The user explicitly authorized recording and waiving this final Rust Gate: “记录并豁免本次最终 Rust Gate，保持 Spec04 范围，完成本地提交并交我验收”. This records an exemption, not a passing test; do not describe all tests as green. Main-thread technical acceptance is complete, and user manual acceptance remains pending. The local commit `feat(api): implement Spec04 HTTP API and Dashboard contracts` is for user review; no production wiring, GitHub write, product cutover, or release was performed.
