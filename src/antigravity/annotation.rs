//! Annotation parser and locator for Antigravity `.pbtxt` files.

use std::{
    fs::File,
    io::{self, Read},
};

use crate::antigravity::config::{AntigravityConfig, is_permission_denied};
use crate::antigravity::protobuf::{MAX_ANNOTATION_BYTES, MAX_TITLE_BYTES};

/// Result of resolving an annotation title.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnnotationTitleResult {
    /// Valid normalized title.
    Valid(String),
    /// Annotation file exists and is valid, but title is absent or trims to empty.
    Empty,
    /// Annotation file does not exist.
    NotFound,
    /// Annotation file is malformed, invalid UTF-8, or oversized.
    Malformed,
    /// File escape, symlink, or permission failure (triggers ANTIGRAVITY_CONFIG_INVALID).
    SecurityEscape(String),
}

/// Read and parse the annotation title for a specific conversation ID.
pub fn read_annotation_title(
    config: &AntigravityConfig,
    conversation_id: &str,
) -> AnnotationTitleResult {
    let annotation_file = config
        .annotations_dir()
        .join(format!("{conversation_id}.pbtxt"));

    // Check symlink_metadata
    let meta = match std::fs::symlink_metadata(&annotation_file) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return AnnotationTitleResult::NotFound;
        }
        Err(err) if is_permission_denied(&err) => {
            return AnnotationTitleResult::SecurityEscape(format!(
                "permission denied reading annotation metadata: {err}"
            ));
        }
        Err(err) => return AnnotationTitleResult::SecurityEscape(err.to_string()),
    };

    if meta.file_type().is_symlink() {
        return AnnotationTitleResult::SecurityEscape(format!(
            "annotation file cannot be a symlink: {}",
            annotation_file.display()
        ));
    }

    if !meta.is_file() {
        return AnnotationTitleResult::SecurityEscape(format!(
            "annotation path is not a file: {}",
            annotation_file.display()
        ));
    }

    // Canonical containment check [INV-SRC-04]
    let canonical = match annotation_file.canonicalize() {
        Ok(c) => c,
        Err(err) if is_permission_denied(&err) => {
            return AnnotationTitleResult::SecurityEscape(format!(
                "permission denied canonicalizing annotation: {err}"
            ));
        }
        Err(err) => return AnnotationTitleResult::SecurityEscape(err.to_string()),
    };

    if !config.contains_canonical_path(&canonical) {
        return AnnotationTitleResult::SecurityEscape(format!(
            "annotation file {} escapes canonical root {}",
            canonical.display(),
            config.canonical_root().display()
        ));
    }

    // Fast preflight length check
    if meta.len() > MAX_ANNOTATION_BYTES {
        return AnnotationTitleResult::Malformed;
    }

    // Bounded read with .take(MAX_ANNOTATION_BYTES + 1) to guard against TOCTOU growth
    let file = match File::open(&annotation_file) {
        Ok(f) => f,
        Err(err) if is_permission_denied(&err) => {
            return AnnotationTitleResult::SecurityEscape(format!(
                "permission denied opening annotation: {err}"
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return AnnotationTitleResult::NotFound;
        }
        Err(err) => return AnnotationTitleResult::SecurityEscape(err.to_string()),
    };

    let mut bounded_reader = file.take(MAX_ANNOTATION_BYTES + 1);
    let mut buffer = Vec::new();
    if let Err(_) = bounded_reader.read_to_end(&mut buffer) {
        return AnnotationTitleResult::Malformed;
    }

    if buffer.len() > MAX_ANNOTATION_BYTES as usize {
        return AnnotationTitleResult::Malformed;
    }

    parse_annotation_title_content(&buffer)
}

/// Parse the title string out of raw annotation pbtxt bytes.
pub fn parse_annotation_title_content(buffer: &[u8]) -> AnnotationTitleResult {
    let content = match std::str::from_utf8(buffer) {
        Ok(c) => c,
        Err(_) => return AnnotationTitleResult::Malformed,
    };

    match extract_title_from_pbtxt(content) {
        None if contains_title_field(content) => AnnotationTitleResult::Malformed,
        None => AnnotationTitleResult::Empty,
        Some(raw_title) => {
            let trimmed = raw_title.trim();
            if trimmed.is_empty() {
                AnnotationTitleResult::Empty
            } else if trimmed.len() > MAX_TITLE_BYTES || trimmed.chars().any(char::is_control) {
                AnnotationTitleResult::Malformed
            } else {
                AnnotationTitleResult::Valid(trimmed.to_string())
            }
        }
    }
}

fn contains_title_field(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    for index in 0..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if byte == b'"' {
            in_string = true;
            continue;
        }
        if bytes.get(index..index + 5) == Some(b"title") {
            let prefix_is_identifier =
                index > 0 && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
            let suffix_is_identifier = bytes
                .get(index + 5)
                .is_some_and(|suffix| suffix.is_ascii_alphanumeric() || *suffix == b'_');
            if !prefix_is_identifier && !suffix_is_identifier {
                return true;
            }
        }
    }
    false
}

/// Extract title string from protobuf text format (e.g. `title: "..."` or `title:"..."`).
fn extract_title_from_pbtxt(text: &str) -> Option<String> {
    let mut remaining = text;
    while let Some(idx) = remaining.find("title") {
        // Match the field token, not an identifier that merely contains the
        // word (for example `subtitle`).
        if remaining[..idx]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            remaining = &remaining[idx + 5..];
            continue;
        }
        let after_key = &remaining[idx + 5..];
        let mut chars = after_key.char_indices();
        // Skip whitespace
        let mut colon_found = false;
        let mut after_colon_offset = 0;
        for (c_idx, ch) in chars.by_ref() {
            if ch.is_whitespace() {
                continue;
            }
            if ch == ':' {
                colon_found = true;
                after_colon_offset = c_idx + 1;
                break;
            }
            break;
        }

        if !colon_found {
            remaining = &remaining[idx + 5..];
            continue;
        }

        let after_colon = &after_key[after_colon_offset..];
        let mut quote_offset = None;
        for (c_idx, ch) in after_colon.char_indices() {
            if ch.is_whitespace() {
                continue;
            }
            if ch == '"' {
                quote_offset = Some(c_idx + 1);
                break;
            }
            break;
        }

        let content_start = match quote_offset {
            Some(o) => o,
            None => {
                remaining = &after_key[after_colon_offset..];
                continue;
            }
        };

        let in_quotes = &after_colon[content_start..];
        let mut parsed_str = String::new();
        let mut escaped = false;
        let mut end_found = false;

        for ch in in_quotes.chars() {
            if escaped {
                match ch {
                    'n' => parsed_str.push('\n'),
                    'r' => parsed_str.push('\r'),
                    't' => parsed_str.push('\t'),
                    '\\' => parsed_str.push('\\'),
                    '"' => parsed_str.push('"'),
                    other => {
                        parsed_str.push('\\');
                        parsed_str.push(other);
                    }
                }
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                end_found = true;
                break;
            } else {
                parsed_str.push(ch);
            }
        }

        if end_found {
            return Some(parsed_str);
        }

        remaining = &after_colon[content_start..];
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_test_config() -> (AntigravityConfig, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let c = COUNTER.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let home = std::env::temp_dir().join(format!("ag-annot-test-{}-{}", t, c));
        fs::create_dir_all(home.join("annotations")).unwrap();
        let cfg = match AntigravityConfig::from_home(&home) {
            crate::antigravity::AntigravityConfigResolution::Ready(c) => c,
            _ => panic!("failed to create test config"),
        };
        (cfg, home)
    }

    #[test]
    fn test_extract_title_variants() {
        assert_eq!(
            extract_title_from_pbtxt("title: \"My Title\""),
            Some("My Title".into())
        );
        assert_eq!(
            extract_title_from_pbtxt("title:\"Unspaced Title\""),
            Some("Unspaced Title".into())
        );
        assert_eq!(
            extract_title_from_pbtxt("last_view:{}\ntitle: \"With Prefix\"\n"),
            Some("With Prefix".into())
        );
        assert_eq!(
            extract_title_from_pbtxt("title: \"With \\\"quotes\\\" and \\\\ slash\""),
            Some("With \"quotes\" and \\ slash".into())
        );
        assert_eq!(extract_title_from_pbtxt("last_view:{}"), None);
        assert_eq!(
            parse_annotation_title_content(b"title: \"unterminated"),
            AnnotationTitleResult::Malformed
        );
        assert_eq!(
            parse_annotation_title_content(b"subtitle: \"not a title\""),
            AnnotationTitleResult::Empty
        );
    }

    #[test]
    fn test_td_p1_annotation_01_matrix() {
        let (cfg, home) = temp_test_config();
        let cid = "effa6389-921a-497e-87e0-5a2962526c07";
        let path = cfg.annotations_dir().join(format!("{cid}.pbtxt"));

        // 1. Missing file -> NotFound
        assert_eq!(
            read_annotation_title(&cfg, cid),
            AnnotationTitleResult::NotFound
        );

        // 2. Valid nonempty
        fs::write(&path, b"title: \"  Valid Title  \"\n").unwrap();
        assert_eq!(
            read_annotation_title(&cfg, cid),
            AnnotationTitleResult::Valid("Valid Title".into())
        );

        // 3. Valid file but empty title
        fs::write(&path, b"title: \"    \"\n").unwrap();
        assert_eq!(
            read_annotation_title(&cfg, cid),
            AnnotationTitleResult::Empty
        );

        // 4. Malformed / oversized
        let big = vec![b'a'; (MAX_ANNOTATION_BYTES + 10) as usize];
        fs::write(&path, &big).unwrap();
        assert_eq!(
            read_annotation_title(&cfg, cid),
            AnnotationTitleResult::Malformed
        );

        // 5. Title with control character
        fs::write(&path, b"title: \"Bad\x00Title\"\n").unwrap();
        assert_eq!(
            read_annotation_title(&cfg, cid),
            AnnotationTitleResult::Malformed
        );

        let _ = fs::remove_dir_all(&home);
    }
}
