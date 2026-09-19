use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{Client, StatusCode, header};
use serde_json::Value;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let base = std::env::temp_dir();
        for attempt in 0..16 {
            let path = base.join(format!(
                "usagi-distribution-runtime-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a distribution runtime fixture directory",
        ))
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
    fn spawn(binary: &Path, runtime_root: &Path) -> io::Result<Self> {
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
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        command
            .env("USAGI_WINDOWS_HEADLESS_SMOKE", "1")
            .env("USERPROFILE", &home)
            .env("APPDATA", home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", home.join("AppData/Local"));
        Ok(Self(Some(command.spawn()?)))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("distribution runtime child exists")
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

fn extract_quoted_values<'a>(document: &'a str, marker: &str) -> Vec<&'a str> {
    document
        .split(marker)
        .skip(1)
        .filter_map(|tail| tail.split('"').next())
        .filter(|value| value.starts_with('/'))
        .collect()
}

fn extract_css_urls(document: &str) -> Vec<&str> {
    document
        .split("url(")
        .skip(1)
        .filter_map(|tail| tail.split(')').next())
        .map(str::trim)
        .map(|value| value.trim_matches(['\"', '\'']))
        .filter(|value| value.starts_with('/'))
        .collect()
}

async fn wait_for_health(client: &Client, child: &mut ChildGuard) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.child_mut().try_wait().expect("poll release child") {
            panic!("embedded release binary exited before health check: {status}");
        }
        if let Ok(response) = client.get("http://127.0.0.1:3210/api/health").send().await
            && response.status() == StatusCode::NO_CONTENT
            && response.headers().get("x-usagi-app")
                == Some(&header::HeaderValue::from_static("Usagi"))
            && response.headers().get("x-usagi-version")
                == Some(&header::HeaderValue::from_static(env!("CARGO_PKG_VERSION")))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "embedded binary did not become healthy"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t_dist_005_release_binary_serves_embedded_frontend_outside_repository() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
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
        &manifest_dir,
    );

    let target_binary = target_dir(&manifest_dir)
        .join("release")
        .join(binary_name());
    assert!(
        target_binary.is_file(),
        "release binary was not produced at {}",
        target_binary.display()
    );
    let fixture = TempRoot::new().expect("create distribution runtime fixture");
    let runtime_root = fixture.path().join("runtime");
    fs::create_dir(&runtime_root).expect("create isolated runtime directory");
    let runtime_binary = runtime_root.join(binary_name());
    fs::copy(&target_binary, &runtime_binary).expect("copy release binary to isolated directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&runtime_binary)
            .expect("read copied binary metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&runtime_binary, permissions).expect("mark copied binary executable");
    }
    assert!(!runtime_root.join("frontend/dist").exists());

    let mut child = ChildGuard::spawn(&runtime_binary, &runtime_root)
        .expect("start embedded release binary from isolated directory");
    let client = Client::builder()
        .no_proxy()
        .build()
        .expect("build local HTTP client");
    wait_for_health(&client, &mut child).await;

    let root = client
        .get("http://127.0.0.1:3210/")
        .send()
        .await
        .expect("request embedded index");
    assert_eq!(root.status(), StatusCode::OK);
    assert_eq!(root.headers()[header::CONTENT_TYPE], "text/html");
    assert_eq!(root.headers()[header::CACHE_CONTROL], "no-cache");
    let index = root.text().await.expect("read embedded index");
    let scripts = extract_quoted_values(&index, "src=\"");
    let stylesheets = extract_quoted_values(&index, "href=\"");
    let script = scripts
        .iter()
        .find(|path| path.ends_with(".js"))
        .copied()
        .expect("embedded index references a JavaScript asset");
    let stylesheet = stylesheets
        .iter()
        .find(|path| path.ends_with(".css"))
        .copied()
        .expect("embedded index references a CSS asset");

    let script_response = client
        .get(format!("http://127.0.0.1:3210{script}"))
        .send()
        .await
        .expect("request embedded JavaScript");
    assert_eq!(script_response.status(), StatusCode::OK);
    assert!(
        script_response.headers()[header::CONTENT_TYPE]
            .to_str()
            .expect("JavaScript content type")
            .starts_with("text/javascript")
    );
    assert!(
        script_response.headers()[header::CACHE_CONTROL]
            .to_str()
            .expect("JavaScript cache control")
            .contains("immutable")
    );

    let stylesheet_response = client
        .get(format!("http://127.0.0.1:3210{stylesheet}"))
        .send()
        .await
        .expect("request embedded CSS");
    assert_eq!(stylesheet_response.status(), StatusCode::OK);
    assert_eq!(
        stylesheet_response.headers()[header::CONTENT_TYPE],
        "text/css"
    );
    assert!(
        stylesheet_response.headers()[header::CACHE_CONTROL]
            .to_str()
            .expect("CSS cache control")
            .contains("immutable")
    );
    let stylesheet_body = stylesheet_response.text().await.expect("read embedded CSS");
    for font in extract_css_urls(&stylesheet_body)
        .into_iter()
        .filter(|path| path.ends_with(".woff2"))
    {
        let font_response = client
            .get(format!("http://127.0.0.1:3210{font}"))
            .send()
            .await
            .expect("request embedded font");
        assert_eq!(font_response.status(), StatusCode::OK);
        assert_eq!(font_response.headers()[header::CONTENT_TYPE], "font/woff2");
    }

    let deep_link = client
        .get("http://127.0.0.1:3210/dashboard/deep/link")
        .send()
        .await
        .expect("request SPA deep link");
    assert_eq!(deep_link.status(), StatusCode::OK);
    assert_eq!(deep_link.headers()[header::CONTENT_TYPE], "text/html");
    assert_eq!(deep_link.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(deep_link.text().await.expect("read SPA fallback"), index);

    let unknown_api = client
        .get("http://127.0.0.1:3210/api/does-not-exist")
        .send()
        .await
        .expect("request unknown API");
    assert_eq!(unknown_api.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        unknown_api.headers()[header::CONTENT_TYPE],
        "application/json"
    );
    let error: Value = unknown_api.json().await.expect("decode API 404 JSON");
    assert_eq!(error["error"]["code"], "NOT_FOUND");
}
