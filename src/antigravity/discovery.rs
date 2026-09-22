//! Discovery and inventory of Antigravity conversations.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::antigravity::config::{AntigravityConfig, is_permission_denied};
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

    // 1. Read summary snapshot if summary DB exists
    let summary_map = if config.summaries_path().exists() {
        let conn = open_external_db(config.summaries_path())?;
        let map = read_summary_snapshot(&conn, cancellation)?;
        if cancellation.load(Ordering::Acquire) {
            return Err(ReaderError::Cancelled);
        }
        map
    } else {
        DiscoveredSummaryMap::default()
    };

    // 2. Scan conversations_dir if it exists
    if !config.conversations_dir().exists() {
        return Ok(DiscoveryInventory {
            conversations: Vec::new(),
            summary_map,
        });
    }

    let entries = fs::read_dir(config.conversations_dir()).map_err(|err| {
        if is_permission_denied(&err) {
            ReaderError::Invalid(format!(
                "permission denied reading conversations directory: {err}"
            ))
        } else {
            ReaderError::Invalid(format!("failed to read conversations directory: {err}"))
        }
    })?;

    let mut discovered_files = Vec::new();
    for entry in entries {
        if cancellation.load(Ordering::Relaxed) {
            return Err(ReaderError::Cancelled);
        }
        let entry = entry.map_err(|err| ReaderError::Invalid(err.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("db") {
            // Verify path containment
            let meta =
                fs::symlink_metadata(&path).map_err(|err| ReaderError::Invalid(err.to_string()))?;
            let canonical = path
                .canonicalize()
                .map_err(|err| ReaderError::Invalid(err.to_string()))?;
            if !config.contains_canonical_path(&canonical) {
                return Err(ReaderError::Invalid(format!(
                    "conversation DB {} escapes canonical root",
                    canonical.display()
                )));
            }
            if meta.file_type().is_symlink() {
                return Err(ReaderError::Invalid(format!(
                    "conversation DB path cannot be a symlink: {}",
                    path.display()
                )));
            }
            if !meta.is_file() {
                return Err(ReaderError::Invalid(format!(
                    "conversation DB path is not a file: {}",
                    path.display()
                )));
            }
            discovered_files.push(path);
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
