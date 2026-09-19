use std::{
    fs, io,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{Router, http::StatusCode, routing::get};
use reqwest::{Client, header};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-distribution-launcher-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn spawn(binary: &Path, runtime_root: &Path, capture: bool) -> io::Result<Self> {
        let home = runtime_root.join("home");
        let codex_home = home.join(".").join("codex");
        fs::create_dir_all(codex_home.join("sessions"))?;
        fs::create_dir_all(codex_home.join("archived_sessions"))?;

        let mut command = Command::new(binary);
        command
            .current_dir(runtime_root)
            .env("HOME", &home)
            .env("CODEX_HOME", &codex_home)
            .env("USAGI_DISABLE_BROWSER", "1")
            .stdin(Stdio::null())
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        #[cfg(windows)]
        command
            .env("USAGI_WINDOWS_HEADLESS_SMOKE", "1")
            .env("USERPROFILE", &home)
            .env("APPDATA", home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", home.join("AppData/Local"));
        Ok(Self(Some(command.spawn()?)))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("launcher child exists")
    }

    fn wait_output(mut self) -> io::Result<Output> {
        self.0
            .take()
            .expect("launcher child exists")
            .wait_with_output()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

async fn wait_for_exit(mut child: ChildGuard) -> Output {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if child
            .child_mut()
            .try_wait()
            .expect("poll launcher child")
            .is_some()
        {
            return child.wait_output().expect("collect launcher output");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "launcher child did not exit within the lifecycle smoke timeout"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn command_output(program: &str, args: &[&str], cwd: &Path) -> Output {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|error| panic!("could not run {program}: {error}"));
    assert!(
        output.status.success(),
        "{program} {args:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn binary_name() -> &'static str {
    if cfg!(windows) { "usagi.exe" } else { "usagi" }
}

fn target_dir(manifest_dir: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                manifest_dir.join(path)
            }
        }
        None => manifest_dir.join("target"),
    }
}

fn build_release_binary(manifest_dir: &Path) -> PathBuf {
    let frontend_dir = manifest_dir.join("frontend");
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    command_output(npm, &["ci"], &frontend_dir);
    command_output(npm, &["run", "build"], &frontend_dir);
    command_output(
        "cargo",
        &[
            "build",
            "--release",
            "--locked",
            "--features",
            "embedded-frontend",
        ],
        manifest_dir,
    );
    let binary = target_dir(manifest_dir).join("release").join(binary_name());
    assert!(
        binary.is_file(),
        "release binary missing at {}",
        binary.display()
    );
    binary
}

fn assert_port_is_free() {
    let address: SocketAddr = "127.0.0.1:3210".parse().unwrap();
    match TcpListener::bind(address) {
        Ok(listener) => drop(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            panic!("T-DIST-006 requires 127.0.0.1:3210 to be free before smoke")
        }
        Err(error) => panic!("could not confirm that 127.0.0.1:3210 is free: {error}"),
    }
}

async fn wait_for_health(client: &Client, child: &mut ChildGuard, port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let url = format!("http://127.0.0.1:{port}/api/health");
    loop {
        if let Some(status) = child.child_mut().try_wait().expect("poll launcher child") {
            panic!("launcher child exited before health check on port {port}: {status}");
        }
        if let Ok(response) = client.get(&url).send().await
            && response.status() == StatusCode::NO_CONTENT
            && response.headers().get("x-usagi-app")
                == Some(&header::HeaderValue::from_static("Usagi"))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "launcher child did not become healthy on port {port}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t_dist_006_launcher_lifecycle_matrix() {
    assert_port_is_free();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let binary = build_release_binary(&manifest_dir);
    let fixture = TempRoot::new().expect("create launcher fixture");
    let runtime_root = fixture.path().join("runtime");
    fs::create_dir(&runtime_root).expect("create launcher runtime directory");
    let runtime_binary = runtime_root.join(binary_name());
    fs::copy(&binary, &runtime_binary).expect("copy release binary into runtime fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&runtime_binary)
            .expect("read runtime binary metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&runtime_binary, permissions).expect("mark runtime binary executable");
    }

    assert_port_is_free();
    let mut first = ChildGuard::spawn(&runtime_binary, &runtime_root, false)
        .expect("start first launcher child");
    let client = Client::builder()
        .no_proxy()
        .build()
        .expect("build local launcher client");
    wait_for_health(&client, &mut first, 3210).await;

    let second = ChildGuard::spawn(&runtime_binary, &runtime_root, true)
        .expect("start duplicate launcher child");
    let duplicate_output = wait_for_exit(second).await;
    assert!(duplicate_output.status.success());
    assert!(
        String::from_utf8_lossy(&duplicate_output.stdout).contains("already running"),
        "duplicate launcher did not use existing-instance path: {}",
        String::from_utf8_lossy(&duplicate_output.stdout)
    );
    let first_still_healthy = client
        .get("http://127.0.0.1:3210/api/health")
        .send()
        .await
        .expect("probe first launcher after duplicate exit");
    assert_eq!(first_still_healthy.status(), StatusCode::NO_CONTENT);
    drop(first);
    assert_port_is_free();

    let occupied_listener = tokio::net::TcpListener::bind("127.0.0.1:3210")
        .await
        .expect("reserve listener for non-Usagi conflict");
    let fake_app = Router::new().route("/api/health", get(|| async { StatusCode::NO_CONTENT }));
    let fake_server = tokio::spawn(axum::serve(occupied_listener, fake_app).into_future());
    #[cfg(windows)]
    {
        let mut fallback = ChildGuard::spawn(&runtime_binary, &runtime_root, true)
            .expect("start Windows launcher against occupied preferred port");
        wait_for_health(&client, &mut fallback, 3211).await;

        let duplicate_fallback = ChildGuard::spawn(&runtime_binary, &runtime_root, true)
            .expect("start duplicate Windows launcher while fallback instance is running");
        let duplicate_fallback_output = wait_for_exit(duplicate_fallback).await;
        assert!(
            duplicate_fallback_output.status.success(),
            "duplicate fallback launcher should reuse the existing Usagi instance"
        );
        let fallback_still_healthy = client
            .get("http://127.0.0.1:3211/api/health")
            .send()
            .await
            .expect("probe fallback launcher after duplicate exit");
        assert_eq!(fallback_still_healthy.status(), StatusCode::NO_CONTENT);

        drop(fallback);
    }

    #[cfg(not(windows))]
    {
        let conflicting = ChildGuard::spawn(&runtime_binary, &runtime_root, true)
            .expect("start launcher against non-Usagi listener");
        let conflict_output = wait_for_exit(conflicting).await;
        assert!(!conflict_output.status.success());
        let diagnostics = format!(
            "{}\n{}",
            String::from_utf8_lossy(&conflict_output.stdout),
            String::from_utf8_lossy(&conflict_output.stderr)
        );
        assert!(diagnostics.contains("already in use by another program"));
        assert!(!diagnostics.contains("panicked at"));
    }

    fake_server.abort();
    let _ = fake_server.await;
}
