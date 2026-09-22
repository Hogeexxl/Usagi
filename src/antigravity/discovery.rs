//! Discovery and inventory of Antigravity conversations.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::antigravity::config::{AntigravityConfig, is_permission_denied};
use crate::antigravity::reader::{
    DiscoveredSummaryMap, ReaderError, open_external_db, read_summary_snapshot,
};

/// A discovered conversation database and its validated intrinsic identity.
#[derive(Clone, Debug)]
pub struct DiscoveredConversation {
    pub db_path: PathBuf,
    pub conversation_id: String,
    pub summary_row: Option<crate::antigravity::normalization::DiscoveredSummaryRow>,
}

/// Inventory of discovered conversations for a scan run.
#[derive(Clone, Debug)]
pub struct DiscoveryInventory {
    pub conversations: Vec<DiscoveredConversation>,
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
        read_summary_snapshot(&conn, cancellation)?
    } else {
        DiscoveredSummaryMap::default()
    };

    // 2. Scan conversations_dir if it exists
    if !config.conversations_dir().exists() {
        return Ok(DiscoveryInventory {
            conversations: Vec::new(),
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
            if meta.is_file() {
                discovered_files.push(path);
            }
        }
    }

    // Sort paths for deterministic discovery ordering
    discovered_files.sort();

    let mut seen_identities: HashMap<String, PathBuf> = HashMap::new();
    let mut conversations = Vec::new();

    for db_path in discovered_files {
        if cancellation.load(Ordering::Relaxed) {
            return Err(ReaderError::Cancelled);
        }

        let conn = open_external_db(&db_path)?;
        let intrinsic_id = crate::antigravity::reader::read_intrinsic_id(&conn)?;

        // [INV-ID-03] Conversation identity conflict
        if let Some(existing_path) = seen_identities.get(&intrinsic_id) {
            return Err(ReaderError::ConversationIdConflict(format!(
                "duplicate intrinsic conversation ID {intrinsic_id} in {} and {}",
                existing_path.display(),
                db_path.display()
            )));
        }
        seen_identities.insert(intrinsic_id.clone(), db_path.clone());

        let summary_row = summary_map.rows.get(&intrinsic_id).cloned();

        conversations.push(DiscoveredConversation {
            db_path,
            conversation_id: intrinsic_id,
            summary_row,
        });
    }

    Ok(DiscoveryInventory { conversations })
}
