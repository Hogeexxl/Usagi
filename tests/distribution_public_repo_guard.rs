use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tracked_paths(root: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("run git ls-files for the public-repository guard");
    assert!(
        output.status.success(),
        "git ls-files failed with {}",
        output.status
    );
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| root.join(String::from_utf8_lossy(path).as_ref()))
        .collect()
}

fn marker(parts: &[&str]) -> String {
    parts.concat()
}

fn text_file(path: &Path) -> Option<String> {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    String::from_utf8(bytes).ok()
}

#[test]
fn t_dist_010_public_repository_static_guard() {
    let root = repository_root();
    let tracked = tracked_paths(&root);

    // Assemble machine-specific markers so this guard cannot match its own
    // source while checking the complete tracked tree.
    let private_home = marker(&["/", "Users", "/", "hogee"]);
    let private_checkout_tail = marker(&["Desktop", "/", "Usagi"]);
    let saved_cargo_config = marker(&[".cargo", "/", "config.toml", ".saved"]);
    let private_key_header = marker(&["BEGIN ", "PRIVATE ", "KEY"]);
    let secret_prefixes = [
        marker(&["s", "k", "-"]),
        marker(&["g", "h", "p", "_"]),
        marker(&["g", "i", "t", "h", "u", "b", "_", "p", "a", "t", "_"]),
        marker(&["x", "o", "x", "b", "-"]),
        marker(&["A", "K", "I", "A"]),
    ];

    let mut files_checked = 0;
    for path in &tracked {
        let relative = path
            .strip_prefix(&root)
            .expect("tracked path belongs to repository")
            .to_string_lossy();
        let relative_lower = relative.to_ascii_lowercase();
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();

        assert_ne!(
            relative, saved_cargo_config,
            "local Cargo backup is tracked"
        );
        assert!(
            !relative_lower.contains("/private/")
                && !relative_lower.contains("/secrets/")
                && !relative_lower.contains("/credentials/"),
            "private fixture or credential directory is tracked: {relative}"
        );
        assert!(
            !(file_name == ".env"
                || (file_name.starts_with(".env.") && file_name != ".env.example")),
            "private environment file is tracked: {relative}"
        );

        if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("sqlite" | "sqlite3" | "db")
        ) {
            panic!("database file is tracked: {relative}");
        }
        if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            panic!("rollout JSONL fixture is tracked: {relative}");
        }

        let Some(contents) = text_file(path) else {
            files_checked += 1;
            continue;
        };
        assert!(
            !contents.contains(&private_home),
            "personal home path appears in tracked file: {relative}"
        );
        assert!(
            !contents.contains(&private_checkout_tail),
            "personal checkout path appears in tracked file: {relative}"
        );
        assert!(
            !contents.contains(&private_key_header),
            "private-key material appears in tracked file: {relative}"
        );
        for prefix in &secret_prefixes {
            assert!(
                !contents.contains(prefix),
                "credential-like token appears in tracked file: {relative}"
            );
        }
        files_checked += 1;
    }
    assert!(
        files_checked > 0,
        "public-repository guard found no tracked files"
    );

    let readme = fs::read_to_string(root.join("README.md")).expect("read public README");
    for required in [
        "Windows 10/11 x64",
        "macOS Apple Silicon arm64",
        "127.0.0.1:3210",
        "CODEX_HOME",
        "mu.sqlite3",
        "每 4 小时",
        "检查更新",
        "版本升级",
        "不需要安装 Rust",
        "Node.js",
        "SQLite",
        "Visual Studio",
        "npm run test",
        "cargo test --locked",
    ] {
        assert!(
            readme.contains(required),
            "README is missing required release fact: {required}"
        );
    }

    for forbidden in [
        "macOS Intel x64",
        "Usagi-v0.1.0-macos-x64.dmg",
        "macos-15-intel",
        "x86_64 dmg",
    ] {
        assert!(
            !readme.contains(forbidden),
            "README must not advertise unsupported macOS Intel targets: {forbidden}"
        );
    }

    let license = fs::read_to_string(root.join("LICENSE")).expect("read MIT license");
    for required in [
        "MIT License",
        "Copyright (c) 2026 Hogeexxl",
        "Permission is hereby granted, free of charge",
        "The above copyright notice and this permission notice",
        "THE SOFTWARE IS PROVIDED \"AS IS\"",
    ] {
        assert!(
            license.contains(required),
            "MIT license is incomplete: {required}"
        );
    }

    let gitignore = fs::read_to_string(root.join(".gitignore")).expect("read public gitignore");
    for required in [
        ".env",
        "*.sqlite3",
        "frontend/node_modules/",
        "frontend/dist/",
    ] {
        assert!(
            gitignore.lines().any(|line| line.trim() == required),
            ".gitignore is missing required privacy/build entry: {required}"
        );
    }

    for variable in ["HOME", "USERPROFILE"] {
        if let Some(value) = env::var_os(variable)
            && let Some(value) = value.to_str()
            && value.len() > 1
        {
            for path in &tracked {
                if let Some(contents) = text_file(path) {
                    assert!(
                        !contents.contains(value),
                        "current {variable} value appears in tracked file: {}",
                        path.display()
                    );
                }
            }
        }
    }
}
