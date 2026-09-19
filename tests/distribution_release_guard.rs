use std::{fs, path::PathBuf};

fn workflow() -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .expect("read release workflow")
    .replace("\r\n", "\n")
}

fn windows_smoke() -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/windows-release-smoke.ps1"),
    )
    .expect("read Windows release smoke")
    .replace("\r\n", "\n")
}

#[test]
fn t_dist_012_release_workflow_is_tag_gated_and_allows_explicit_dispatch() {
    let release = workflow();
    let windows_smoke = windows_smoke();
    assert!(release.contains("on:\n  push:\n    tags:\n      - 'v*.*.*'"));
    assert!(release.contains("workflow_dispatch:"));
    assert!(!release.contains("pull_request:"));
    assert!(!release.contains("branches:"));
    assert!(release.contains("permissions:\n  contents: read"));
    assert!(release.contains("permissions:\n      contents: write"));
    assert!(
        release
            .matches("cargo metadata --no-deps --format-version 1")
            .count()
            >= 3
    );
    assert!(release.contains("^v(?<base>\\d+\\.\\d+\\.\\d+)(?<suffix>-rc\\.\\d+)?$"));
    assert!(release.matches("(-rc\\.[0-9]+)?$").count() >= 2);
    assert!(release.contains("$tagBase -ne $cargoVersion"));
    assert!(release.contains("tag_base=\"${BASH_REMATCH[1]}\""));
    assert!(release.contains("Stable release tag"));
    assert!(release.contains("${GITHUB_REF_NAME#v}"));
    assert!(release.contains("GITHUB_REPOSITORY"));
    assert!(release.contains("secrets.GITHUB_TOKEN"));
    assert!(!release.contains("secrets.PAT"));
    assert!(!release.contains("softprops/action-gh-release"));
    assert!(!release.contains("actions/upload-release-asset"));
    assert!(release.contains("sha256sum"));
    assert!(release.contains("SHA256SUMS.txt"));
    assert!(release.contains("gh release create"));
    assert!(release.contains("--verify-tag"));
    assert!(release.contains("release_args=(--verify-tag)"));
    assert!(release.contains("release_args+=(--draft --prerelease)"));
    assert!(release.contains("is_candidate=\"${{ steps.version.outputs.is_candidate }}\""));
    assert!(release.contains("\"${release_args[@]}\""));
    assert!(release.contains("windows_name=\"Usagi-v${version}-windows-x64-setup.exe\""));
    assert!(release.contains("macos_name=\"Usagi-v${version}-macos-arm64.dmg\""));
    assert!(release.contains("TAG_VERSION=$tagVersion"));
    assert!(release.contains("CARGO_VERSION=$cargoVersion"));
    assert!(windows_smoke.contains("expectedBinaryVersion = $env:CARGO_VERSION"));
    assert!(release.contains("X-Usagi-Version"));
}

#[test]
fn t_dist_012_release_jobs_build_only_supported_assets() {
    let release = workflow();
    for required in [
        "runs-on: windows-latest",
        "runs-on: macos-latest",
        "test \"$(uname -m)\" = \"arm64\"",
        "npm ci",
        "npm run build",
        "cargo test --locked",
        "cargo build --release --locked --features embedded-frontend",
        "cargo install cargo-packager --locked --version",
        "cargo packager --release --formats nsis",
        "cargo packager --release --formats dmg",
        "Usagi-v$env:TAG_VERSION-windows-x64-setup.exe",
        "Usagi-v${TAG_VERSION}-macos-arm64.dmg",
        "actions/upload-artifact@v4",
        "actions/download-artifact@v4",
    ] {
        assert!(
            release.contains(required),
            "release workflow is missing: {required}"
        );
    }
    for forbidden in [
        "macos-15-intel",
        "macOS Intel",
        "x86_64.dmg",
        "macos-x64.dmg",
        "notarytool",
        "codesign",
        "resources =",
        "frontend/dist",
    ] {
        assert!(
            !release.contains(forbidden),
            "release workflow must not include unsupported or external resources: {forbidden}"
        );
    }
}

#[test]
fn t_dist_013_windows_release_has_static_runtime_and_install_smoke() {
    let release = workflow();
    let smoke = windows_smoke();

    for required in [
        "target-feature=+crt-static",
        "VCRUNTIME",
        "MSVCP",
        "/DEPENDENTS",
        "llvm-readobj.exe",
        "Inspect Windows PE imports and GUI subsystem",
        "Windows GUI",
        "IMAGE_SUBSYSTEM_WINDOWS_GUI",
        "IMAGE_FILE_MACHINE_AMD64",
        "PE32+",
        "expectedPackagerName = \"usagi_$($env:CARGO_VERSION)_x64-setup.exe\"",
        "$generated[0].Name -cne $expectedPackagerName",
        "T-DIST-013 clean-runtime installer smoke",
        ".github/scripts/windows-release-smoke.ps1",
    ] {
        assert!(
            release.contains(required),
            "Windows packaging workflow guard is missing: {required}"
        );
    }

    for required in [
        "Start-Process -FilePath $installer",
        "'/S'",
        "USAGI_WINDOWS_HEADLESS_SMOKE",
        "USAGI_DISABLE_BROWSER",
        "Start-Process -FilePath `$binaryPath",
        "-PassThru",
        "-Wait",
        "-RedirectStandardOutput `$stdoutPath",
        "-RedirectStandardError `$stderrPath",
        "exit `$usagi.ExitCode",
        "127.0.0.1:3210/api/health",
        "X-Usagi-Version",
        "expectedBinaryVersion = $env:CARGO_VERSION",
        "SkipHttpErrorCheck",
        "Content-Type",
        "spa-route",
        "acceptance-not-found",
        "acceptance-user-data.txt",
        "Get-FileHash",
        "NSIS reinstall",
        "uninstall*.exe",
        "$uninstallers.Count -ne 1",
        "$uninstallerPath",
        "Start-Process -FilePath $uninstallerPath",
        "NSIS uninstall left usagi.exe",
        "databaseHashBeforeUninstall",
        "sentinelHashBeforeUninstall",
        "New-LocalUser",
        "CreateProcessWithLogonW",
        "LOGON_WITH_PROFILE",
        "CREATE_NO_WINDOW",
        "IntPtr.Zero",
        "lpEnvironment",
        "Start-IsolatedUserProcess",
        "Stop-IsolatedProcessTree",
        "Get-Service -Name 'seclogon'",
        "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
        "[Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)",
        "CreateProcessWithLogonW did not run as the isolated Windows user",
        "Installed runtime resolved the wrong Windows LocalApplicationData known folder",
        "$appDataRoot = Join-Path $localAppData 'Usagi'",
        "Windows LocalApplicationData escaped the isolated user profile",
        "Get-CimInstance -ClassName Win32_UserProfile",
        "Remove-CimInstance",
        "Remove-LocalUser",
        "`$env:PATH = \"`$env:SystemRoot\\System32;`$env:SystemRoot\"",
        "Installed runtime unexpectedly contains a frontend directory",
    ] {
        assert!(
            smoke.contains(required),
            "Windows clean-runtime smoke guard is missing: {required}"
        );
    }

    assert!(!smoke.contains("& '$escapedBinary'"));
    assert!(!smoke.contains("exit `$LASTEXITCODE"));

    for forbidden in [
        "if ($null -ne $uninstaller)",
        "Join-Path $home 'AppData/Local'",
        "$dataRoot",
        "Environment['HOME']",
        "Environment['USERPROFILE']",
        "Environment['LOCALAPPDATA']",
        "-Credential $credential",
        "-LoadUserProfile",
        "Register-ScheduledTask",
        "Start-ScheduledTask",
        "Stop-ScheduledTask",
        "Unregister-ScheduledTask",
    ] {
        assert!(
            !smoke.contains(forbidden),
            "Windows clean-runtime smoke must not use obsolete isolation mechanism: {forbidden}"
        );
    }
}

#[test]
fn t_dist_014_macos_release_has_arm64_clean_runtime_smoke() {
    let release = workflow();
    for required in [
        "T-DIST-014 clean-runtime arm64 DMG smoke",
        "hdiutil attach -plist -nobrowse -readonly",
        "find target/release -maxdepth 1 -type f -name '*.dmg'",
        "Usagi_${CARGO_VERSION}_aarch64.dmg",
        "Usagi_${CARGO_VERSION}_arm64.dmg",
        "cargo-packager DMG basename",
        "hdiutil detach",
        "attached_devices=()",
        "attached_mounts=()",
        "for mount in \"${attached_mounts[@]}\"",
        "for device in \"${attached_devices[@]}\"",
        "attach.plist",
        "attach.entities",
        "plistlib",
        "mount_count",
        "trap cleanup EXIT INT TERM",
        "file \"$app/Contents/MacOS/usagi\"",
        "ditto \"$app\" \"$app_copy\"",
        "cd \"$runtime_root\"",
        "PATH=\"/usr/bin:/bin\"",
        "curl --silent --output /dev/null",
        "X-Usagi-App",
        "CARGO_VERSION",
        "expected_binary_version=\"$CARGO_VERSION\"",
        "database_dir=\"$home/Library/Application Support/Usagi\"",
        "database_path=\"$database_dir/mu.sqlite3\"",
        "Configured CODEX_HOME is not an existing readable directory",
        "Content-Type",
        "src|href",
        "text/css",
        "javascript",
        "spa-route",
        "acceptance-not-found",
        "-type d -name frontend",
        "macos-arm64.dmg",
    ] {
        assert!(
            release.contains(required),
            "macOS packaging guard is missing: {required}"
        );
    }
    assert!(!release.contains("hdiutil detach \"$mount_point\""));
    assert!(!release.contains("mount_output="));
    assert!(!release.contains("mount_points="));
}

#[test]
fn packager_metadata_uses_cargo_identity_without_runtime_resources_or_signing() {
    let manifest = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("read Cargo manifest");
    assert!(manifest.contains("[package.metadata.packager]"));
    assert!(manifest.contains("product-name = \"Usagi\""));
    assert!(manifest.contains("identifier = \"com.hogeexxl.usagi\""));
    assert!(manifest.contains("formats = [\"nsis\", \"dmg\"]"));
    assert!(!manifest.contains("resources ="));
    assert!(!manifest.contains("signing-identity"));
    assert!(!manifest.contains("notarization"));
    assert!(!manifest.contains("frontend/dist"));
}
