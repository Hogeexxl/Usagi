import { spawn, spawnSync, execFileSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const frontendDir = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const repoRoot = resolve(frontendDir, "..");
const binary = resolve(repoRoot, "dist/usagi");
const magpieFixture = resolve(repoRoot, "scripts/ci/fixtures/magpie_http.py");
const python = execFileSync("python3", ["-c", "import sys; print(sys.executable)"], { encoding: "utf8" }).trim();
const npmCommand = process.platform === "win32" ? "npm.cmd" : "npm";
const tempRoot = mkdtempSync(resolve(tmpdir(), "usagi-browser-gate-"));
const home = resolve(tempRoot, "home");
const stubDir = resolve(tempRoot, "bin");
const fixtureDir = resolve(tempRoot, "magpie");
const outsideDir = resolve(tempRoot, "outside");
const databaseDir = resolve(home, "Library/Application Support/Usagi");
const databasePath = resolve(databaseDir, "usagi.sqlite");
const descriptorPath = resolve(databaseDir, "runtime.json");
const releasePath = resolve(fixtureDir, "release_sync");
const waitingPath = resolve(fixtureDir, "sync_waiting");
const offlinePath = resolve(tempRoot, "magpie-offline");
const pidPath = resolve(fixtureDir, "magpie.pid");
let runtime;
let runtimeLog = "";

function assertRuntimePortsFree() {
  const script = String.raw`import urllib.error
import urllib.request

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
for port in range(37891, 37923):
    try:
        response = opener.open(f"http://127.0.0.1:{port}/api/health", timeout=0.2)
    except (urllib.error.URLError, TimeoutError):
        continue
    with response:
        if 200 <= response.status < 300 and response.headers.get_all("X-Usagi-App", []) == ["Usagi"]:
            raise SystemExit(f"active Usagi instance already uses port {port}; browser gate aborted")`;
  const result = spawnSync(python, ["-c", script], { stdio: "inherit" });
  if (result.status !== 0) throw new Error("runtime port scan failed");
}

function seedDatabase() {
  mkdirSync(databaseDir, { recursive: true, mode: 0o700 });
  const script = String.raw`import pathlib
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[2])
connection.executescript(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
connection.commit()
assert connection.execute("SELECT value FROM app_meta WHERE key='initial_sync_complete'").fetchone()[0] == "0"
connection.close()`;
  execFileSync(python, ["-c", script, resolve(repoRoot, "internal/database/schema.sql"), databasePath], { stdio: "inherit" });
}

function writeMagpieStub() {
  mkdirSync(stubDir, { recursive: true });
  mkdirSync(fixtureDir, { recursive: true });
  mkdirSync(outsideDir, { recursive: true });
  const stubPath = resolve(stubDir, "magpie");
  writeFileSync(stubPath, `#!/bin/sh
case "$1" in
  healthcheck) [ ! -e "$USAGI_MAGPIE_OFFLINE_FILE" ] ;;
  web) shift; exec "$USAGI_MAGPIE_PYTHON" "$USAGI_MAGPIE_FIXTURE_SCRIPT" "$@" ;;
  *) exit 2 ;;
esac
`, { mode: 0o755 });
  chmodSync(stubPath, 0o755);
}

function waitForRuntime() {
  return new Promise((resolvePromise, rejectPromise) => {
    let settled = false;
    const timeout = setTimeout(() => {
      if (!settled) rejectPromise(new Error(`runtime descriptor did not appear\n${runtimeLog}`));
    }, 20_000);
    runtime.on("exit", (code, signal) => {
      if (!settled) {
        settled = true;
        clearTimeout(timeout);
        rejectPromise(new Error(`runtime exited before descriptor (code=${code}, signal=${signal})\n${runtimeLog}`));
      }
    });
    const poll = () => {
      if (existsSync(descriptorPath)) {
        try {
          const descriptor = JSON.parse(readFileSync(descriptorPath, "utf8"));
          if (descriptor.schema_version !== 1 || descriptor.pid !== runtime.pid ||
              descriptor.base_url !== `http://127.0.0.1:${descriptor.port}` ||
              descriptor.port < 37891 || descriptor.port > 37922) {
            throw new Error(`invalid runtime descriptor: ${JSON.stringify(descriptor)}`);
          }
          settled = true;
          clearTimeout(timeout);
          resolvePromise(descriptor);
          return;
        } catch (error) {
          settled = true;
          clearTimeout(timeout);
          rejectPromise(error);
          return;
        }
      }
      if (!settled) setTimeout(poll, 50);
    };
    poll();
  });
}

async function waitForSyncGate() {
  const deadline = Date.now() + 15_000;
  while (!existsSync(waitingPath)) {
    if (runtime.exitCode !== null) throw new Error(`runtime exited while waiting for initial sync\n${runtimeLog}`);
    if (Date.now() >= deadline) throw new Error(`Magpie did not block the initial daily sync\n${runtimeLog}`);
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  }
}

function runBrowserTests(env) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(npmCommand, ["run", "test:browser"], {
      cwd: frontendDir,
      env,
      stdio: "inherit",
      shell: process.platform === "win32" && npmCommand.endsWith(".cmd"),
    });
    child.on("error", rejectPromise);
    child.on("exit", (code, signal) => {
      if (code === 0) resolvePromise(undefined);
      else rejectPromise(new Error(`npm run test:browser exited with code=${code}, signal=${signal}`));
    });
  });
}

async function stopRuntime() {
  if (!runtime || runtime.exitCode !== null) return;
  try {
    const descriptor = existsSync(descriptorPath) ? JSON.parse(readFileSync(descriptorPath, "utf8")) : null;
    if (descriptor?.base_url) {
      await fetch(`${descriptor.base_url}/api/service/stop`, { method: "POST", signal: AbortSignal.timeout(3000) });
    }
  } catch {}
  if (runtime.exitCode === null) {
    runtime.kill("SIGTERM");
    const exited = new Promise((resolvePromise) => {
      if (runtime.exitCode !== null) resolvePromise(undefined);
      else runtime.once("exit", resolvePromise);
    });
    await Promise.race([exited, new Promise((resolvePromise) => setTimeout(resolvePromise, 3000))]);
    if (runtime.exitCode === null) {
      runtime.kill("SIGKILL");
      await exited;
    }
  }
}

function fixtureProcessCommand(pid) {
  try {
    return execFileSync("ps", ["-p", String(pid), "-o", "command="], { encoding: "utf8" }).trim();
  } catch {
    return "";
  }
}

async function stopMagpieFixture() {
  if (!existsSync(pidPath)) return;
  const fixturePid = Number(readFileSync(pidPath, "utf8"));
  if (!Number.isInteger(fixturePid) || fixturePid <= 0) throw new Error("invalid simulated Magpie PID");
  const isFixtureRunning = () => fixtureProcessCommand(fixturePid).includes(magpieFixture);
  if (!isFixtureRunning()) return;
  try {
    process.kill(fixturePid, "SIGTERM");
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
  const waitForExit = async (timeoutMs) => {
    const deadline = Date.now() + timeoutMs;
    while (isFixtureRunning() && Date.now() < deadline) {
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
    }
    return !isFixtureRunning();
  };
  if (await waitForExit(3000)) return;
  try {
    process.kill(fixturePid, "SIGKILL");
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
  if (!(await waitForExit(3000))) throw new Error("simulated Magpie process did not exit before temp cleanup");
}

async function main() {
  if (!existsSync(binary)) throw new Error(`missing executable runtime: ${binary}`);
  assertRuntimePortsFree();
  writeMagpieStub();
  seedDatabase();
  const runtimeEnv = {
    ...process.env,
    HOME: home,
    PATH: `${stubDir}:/usr/bin:/bin:/usr/sbin:/sbin`,
    USAGI_DISABLE_BROWSER: "1",
    USAGI_MAGPIE_FIXTURE_DIR: fixtureDir,
    USAGI_MAGPIE_FIXTURE_SCRIPT: magpieFixture,
    USAGI_MAGPIE_PYTHON: python,
    USAGI_MAGPIE_OFFLINE_FILE: offlinePath,
    USAGI_MAGPIE_BLOCK_DAILY: "1",
  };
  runtime = spawn(binary, [], { cwd: outsideDir, env: runtimeEnv, stdio: ["ignore", "pipe", "pipe"] });
  runtime.stdout.on("data", (chunk) => { runtimeLog += chunk.toString(); });
  runtime.stderr.on("data", (chunk) => { runtimeLog += chunk.toString(); });
  const descriptor = await waitForRuntime();
  await waitForSyncGate();
  const browserEnv = {
    ...process.env,
    FRONTEND_BASE_URL: descriptor.base_url,
    USAGI_BROWSER_RELEASE_FILE: releasePath,
    USAGI_BROWSER_MAGPIE_PID_FILE: pidPath,
  };
  await runBrowserTests(browserEnv);
  console.log("browser gate passed with temporary SQLite, simulated Magpie HTTP, first-sync loading, and offline reads");
}

try {
  await main();
} catch (error) {
  if (runtimeLog) process.stderr.write(`runtime output:\n${runtimeLog}`);
  throw error;
} finally {
  try {
    await stopRuntime();
    await stopMagpieFixture();
  } finally {
    rmSync(tempRoot, { recursive: true, force: true });
  }
}
