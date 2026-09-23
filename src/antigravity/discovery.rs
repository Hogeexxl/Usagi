//! Discovery and inventory of Antigravity conversations.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::antigravity::config::AntigravityConfig;
use crate::antigravity::reader::{
    DiscoveredSummaryMap, ReaderError, open_external_db, read_summary_snapshot,
};

/// A discovered conversation database.
///
/// The intrinsic identity is intentionally not read during discovery.  It is
/// read by `read_conversation_snapshot` inside the same deferred transaction
/// as the usage rows and trajectory workspace.
#[derive(Clone, Debug)]
pub struct DiscoveredConversation {
    pub db_path: PathBuf,
}

/// Inventory of discovered conversations for a scan run.
#[derive(Clone, Debug)]
pub struct DiscoveryInventory {
    pub conversations: Vec<DiscoveredConversation>,
    pub summary_map: DiscoveredSummaryMap,
}

/// Discover all valid conversation databases in the configured Antigravity home.
pub fn discover_inventory(
    config: &AntigravityConfig,
    cancellation: &AtomicBool,
) -> Result<DiscoveryInventory, ReaderError> {
    if cancellation.load(Ordering::Relaxed) {
        return Err(ReaderError::Cancelled);
    }

    // Revalidate the optional summary DB because the config may have been
    // resolved before its path or permissions changed.
    let summary_path = config
        .summaries_path_for_scan()
        .map_err(|err| ReaderError::ConfigInvalid(err.to_string()))?;
    let summary_map = match summary_path {
        Some(path) => {
            let conn = open_external_db(path)?;
            let map = read_summary_snapshot(&conn, cancellation)?;
            if cancellation.load(Ordering::Acquire) {
                return Err(ReaderError::Cancelled);
            }
            map
        }
        None => DiscoveredSummaryMap::default(),
    };

    // Revalidate the optional conversations directory before enumerating it.
    let Some(conversations_dir) = config
        .conversations_dir_for_scan()
        .map_err(|err| ReaderError::ConfigInvalid(err.to_string()))?
    else {
        return Ok(DiscoveryInventory {
            conversations: Vec::new(),
            summary_map,
        });
    };

    let entries = fs::read_dir(&conversations_dir).map_err(|err| {
        ReaderError::ConfigInvalid(format!(
            "failed to read conversations directory {}: {err}",
            conversations_dir.display()
        ))
    })?;

    let mut discovered_files = Vec::new();
    for entry in entries {
        if cancellation.load(Ordering::Relaxed) {
            return Err(ReaderError::Cancelled);
        }
        let entry = entry.map_err(|err| ReaderError::ConfigInvalid(err.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("db") {
            let canonical = config
                .conversation_db_path_for_scan(&path)
                .map_err(|err| ReaderError::ConfigInvalid(err.to_string()))?;
            discovered_files.push(canonical);
        }
    }

    // Sort paths for deterministic discovery ordering
    discovered_files.sort();

    let conversations = discovered_files
        .into_iter()
        .map(|db_path| DiscoveredConversation { db_path })
        .collect();

    Ok(DiscoveryInventory {
        conversations,
        summary_map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::antigravity::config::{ANTIGRAVITY_CONFIG_INVALID, AntigravityConfigResolution};

    fn unique_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{timestamp}-{counter}")
    }

    fn create_temp_home(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("usagi-ag-discovery-{label}-{}", unique_id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn ready_config(home: &PathBuf) -> AntigravityConfig {
        match AntigravityConfig::from_home(home) {
            AntigravityConfigResolution::Ready(config) => config,
            other => panic!("expected Ready config, got {other:?}"),
        }
    }

    fn cancellation() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn missing_optional_scan_paths_are_empty() {
        let home = create_temp_home("missing");
        let config = ready_config(&home);
        let inventory = discover_inventory(&config, &cancellation()).unwrap();

        assert!(inventory.conversations.is_empty());
        assert!(inventory.summary_map.rows.is_empty());
        let _ = fs::remove_dir_all(home);
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_summary_symlink_replaced_after_config_resolution() {
        let home = create_temp_home("summary-swap");
        let external = create_temp_home("summary-outside");
        let summary = home.join("conversation_summaries.db");
        drop(rusqlite::Connection::open(&summary).unwrap());
        let config = ready_config(&home);
        fs::remove_file(&summary).unwrap();
        let external_summary = external.join("conversation_summaries.db");
        drop(rusqlite::Connection::open(&external_summary).unwrap());
        std::os::unix::fs::symlink(&external_summary, &summary).unwrap();

        let err = discover_inventory(&config, &cancellation()).unwrap_err();
        assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
        assert!(err.to_string().contains("escapes canonical root"));

        let _ = fs::remove_dir_all(home);
        let _ = fs::remove_dir_all(external);
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_conversations_directory_swapped_to_external_symlink() {
        let home = create_temp_home("conversations-swap");
        let external = create_temp_home("conversations-outside");
        let conversations = home.join("conversations");
        fs::create_dir(&conversations).unwrap();
        let config = ready_config(&home);
        fs::remove_dir(&conversations).unwrap();
        std::os::unix::fs::symlink(&external, &conversations).unwrap();

        let err = discover_inventory(&config, &cancellation()).unwrap_err();
        assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
        assert!(err.to_string().contains("escapes canonical root"));

        let _ = fs::remove_dir_all(home);
        let _ = fs::remove_dir_all(external);
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_summary_that_becomes_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        struct RestoreGuard(PathBuf);
        impl Drop for RestoreGuard {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o644));
            }
        }

        let home = create_temp_home("summary-mode-change");
        let summary = home.join("conversation_summaries.db");
        drop(rusqlite::Connection::open(&summary).unwrap());
        let config = ready_config(&home);
        let guard = RestoreGuard(summary.clone());
        fs::set_permissions(&summary, fs::Permissions::from_mode(0o000)).unwrap();

        let err = discover_inventory(&config, &cancellation()).unwrap_err();
        assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
        assert!(err.to_string().contains("not readable"));

        drop(guard);
        let _ = fs::remove_dir_all(home);
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_conversations_directory_that_becomes_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        struct RestoreGuard(PathBuf);
        impl Drop for RestoreGuard {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
        }

        let home = create_temp_home("conversations-mode-change");
        let conversations = home.join("conversations");
        fs::create_dir(&conversations).unwrap();
        let config = ready_config(&home);
        let guard = RestoreGuard(conversations.clone());
        fs::set_permissions(&conversations, fs::Permissions::from_mode(0o000)).unwrap();

        let err = discover_inventory(&config, &cancellation()).unwrap_err();
        assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
        assert!(err.to_string().contains("not readable"));

        drop(guard);
        let _ = fs::remove_dir_all(home);
    }
}
