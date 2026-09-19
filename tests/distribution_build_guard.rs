use std::{fs, path::Path};

#[test]
fn repository_does_not_require_vendored_sources() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo_config = manifest_dir.join(".cargo/config.toml");
    if cargo_config.exists() {
        let config = fs::read_to_string(cargo_config).expect("read Cargo configuration");
        assert!(!config.contains("replace-with"));
        assert!(!config.contains("vendored-sources"));
        assert!(!config.contains("directory = \"vendor\""));
    }

    assert!(!manifest_dir.join("vendor").exists());
    assert!(!manifest_dir.join(".cargo/config.toml.saved").exists());
}

#[test]
fn package_version_is_the_cargo_package_version() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("read Cargo.toml");
    let package = manifest
        .split_once("[package]")
        .expect("Cargo.toml has [package]")
        .1
        .split_once("\n[")
        .map_or_else(|| manifest.as_str(), |(section, _)| section);
    let manifest_version = package
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "version").then(|| value.trim().trim_matches('"'))
        })
        .expect("package version exists");

    assert_eq!(env!("CARGO_PKG_VERSION"), manifest_version);
}

#[test]
fn cargo_binary_target_is_usagi() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("read Cargo.toml");

    assert!(manifest.contains("autobins = false"));
    assert!(manifest.contains("[[bin]]"));
    assert!(manifest.contains("name = \"usagi\""));
    assert!(manifest.contains("path = \"src/main.rs\""));
    assert!(!manifest.contains("name = \"mini-usage\"\npath = \"src/main.rs\""));
}

#[test]
fn windows_binary_uses_gui_subsystem_in_debug_and_release() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main = fs::read_to_string(manifest_dir.join("src/main.rs")).expect("read src/main.rs");

    assert!(main.contains(
        "#![cfg_attr(all(target_os = \"windows\", not(test)), windows_subsystem = \"windows\")]"
    ));
    assert!(!main.contains("not(debug_assertions)"));
}
