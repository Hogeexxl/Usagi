//! Fixed-view, guarded rollout chunk reading.
//!
//! Discovery and planning happen outside this module. A read never expands
//! beyond the plan's observed size, and only newline-terminated records leave
//! the reader.

use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::platform::file_identity;

pub const GUARD_WINDOW_BYTES: u64 = 4096;
pub const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;
const READ_BUFFER_BYTES: usize = 64 * 1024;
#[cfg(test)]
pub(crate) const MAX_BUFFERED_BODY_BYTES: u64 = 2 * MAX_LINE_BYTES + READ_BUFFER_BYTES as u64;

pub use crate::platform::PlatformFileIdentity as PhysicalIdentity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuardHash([u8; 32]);

impl GuardHash {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkReadPlan {
    pub path: PathBuf,
    pub identity: PhysicalIdentity,
    pub start_offset: u64,
    pub observed_size: u64,
    pub expected_guard: Option<GuardHash>,
}

/// A complete line including its trailing LF and, for CRLF input, CR.
///
/// Raw bytes are private and the type intentionally has no `Debug`
/// implementation so diagnostics cannot accidentally include rollout text.
pub struct FramedLine {
    start_offset: u64,
    bytes_with_newline: Vec<u8>,
}

impl FramedLine {
    pub const fn start_offset(&self) -> u64 {
        self.start_offset
    }

    #[cfg(test)]
    pub fn json_bytes(&self) -> &[u8] {
        let without_lf = &self.bytes_with_newline[..self.bytes_with_newline.len() - 1];
        without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
    }

    pub fn into_bytes_with_newline(self) -> Vec<u8> {
        self.bytes_with_newline
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineDiagnosticCode {
    OversizedCompleteLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineDiagnostic {
    pub code: LineDiagnosticCode,
    pub start_offset: u64,
    pub end_offset: u64,
}

pub enum FramedItem {
    Line(FramedLine),
    OversizedCompleteLine(LineDiagnostic),
}

pub struct ChunkReadResult {
    pub complete_line_count: u64,
    pub oversized_complete_line_count: u64,
    pub last_complete_offset: u64,
    pub bytes_read: u64,
    pub guard_bytes_read: u64,
    pub peak_buffered_body_bytes: u64,
    pub has_half_line: bool,
    pub fixed_view_exhausted: bool,
    pub guard: Option<GuardHash>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadControl {
    Continue,
    StopAfter,
    StopBefore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOperation {
    SymlinkMetadata,
    Open,
    HandleMetadata,
    Seek,
    Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkReadError {
    SourceSymlinkRejected,
    SourceNotRegularFile,
    SourceChangedBeforeRead,
    SourceChangedDuringRead,
    CheckpointOutOfRange,
    InvalidGuardPlan,
    CheckpointGuardMismatch,
    Io {
        operation: ReadOperation,
        kind: io::ErrorKind,
    },
}

/// Streams provisional complete-line items from the fixed view.
///
/// The caller must retain only safe parsed state until this function returns
/// `Ok`: the final handle/path identity checks happen after the stream ends.
pub fn read_chunk(
    plan: &ChunkReadPlan,
    mut on_item: impl FnMut(FramedItem),
) -> Result<ChunkReadResult, ChunkReadError> {
    read_chunk_bounded(plan, |item| {
        on_item(item);
        ReadControl::Continue
    })
}

/// Variant used by the usage consumer. It may stop only immediately after a
/// complete framed item, while still performing the same final identity/path
/// checks and returning a guard at the exact committed boundary.
pub(crate) fn read_chunk_bounded(
    plan: &ChunkReadPlan,
    mut on_item: impl FnMut(FramedItem) -> ReadControl,
) -> Result<ChunkReadResult, ChunkReadError> {
    validate_plan(plan)?;

    let path_metadata = std::fs::symlink_metadata(&plan.path)
        .map_err(|error| io_error(ReadOperation::SymlinkMetadata, error))?;
    if path_metadata.file_type().is_symlink() {
        return Err(ChunkReadError::SourceSymlinkRejected);
    }
    if !path_metadata.is_file() {
        return Err(ChunkReadError::SourceNotRegularFile);
    }

    let mut file = File::open(&plan.path).map_err(|error| io_error(ReadOperation::Open, error))?;
    let before = file
        .metadata()
        .map_err(|error| io_error(ReadOperation::HandleMetadata, error))?;
    if !before.is_file()
        || file_identity::identity_from_file(&file)
            .map_err(|error| io_error(ReadOperation::HandleMetadata, error))?
            != plan.identity
        || before.len() < plan.observed_size
    {
        return Err(ChunkReadError::SourceChangedBeforeRead);
    }

    let mut guard_bytes_read = verify_guard(&mut file, plan.start_offset, plan.expected_guard)?;
    file.seek(SeekFrom::Start(plan.start_offset))
        .map_err(|error| io_error(ReadOperation::Seek, error))?;

    let requested_bytes = plan.observed_size - plan.start_offset;
    let (mut result, _reached_boundary) = read_fixed_range(
        &mut file,
        plan.start_offset,
        plan.observed_size,
        requested_bytes,
        &mut on_item,
    )?;

    let after = file
        .metadata()
        .map_err(|error| io_error(ReadOperation::HandleMetadata, error))?;
    if !after.is_file()
        || file_identity::identity_from_file(&file)
            .map_err(|error| io_error(ReadOperation::HandleMetadata, error))?
            != plan.identity
        || after.len() < plan.observed_size
        || (after.len() == before.len() && modified_ns(&after) != modified_ns(&before))
    {
        return Err(ChunkReadError::SourceChangedDuringRead);
    }

    let final_path_metadata = std::fs::symlink_metadata(&plan.path)
        .map_err(|_| ChunkReadError::SourceChangedDuringRead)?;
    if final_path_metadata.file_type().is_symlink()
        || !final_path_metadata.is_file()
        || file_identity::identity_from_path(&plan.path)
            .map_err(|_| ChunkReadError::SourceChangedDuringRead)?
            != plan.identity
        || final_path_metadata.len() < plan.observed_size
    {
        return Err(ChunkReadError::SourceChangedDuringRead);
    }

    result.guard = compute_guard(&mut file, result.last_complete_offset).map_err(|error| {
        if is_unexpected_eof(error) {
            ChunkReadError::SourceChangedDuringRead
        } else {
            error
        }
    })?;
    guard_bytes_read =
        guard_bytes_read.saturating_add(guard_window_len(result.last_complete_offset));
    result.guard_bytes_read = guard_bytes_read;
    Ok(result)
}

fn validate_plan(plan: &ChunkReadPlan) -> Result<(), ChunkReadError> {
    if plan.start_offset > plan.observed_size {
        return Err(ChunkReadError::CheckpointOutOfRange);
    }
    match (plan.start_offset, plan.expected_guard) {
        (0, None) | (1.., Some(_)) => Ok(()),
        _ => Err(ChunkReadError::InvalidGuardPlan),
    }
}

fn verify_guard(
    file: &mut File,
    offset: u64,
    expected: Option<GuardHash>,
) -> Result<u64, ChunkReadError> {
    let actual = compute_guard(file, offset).map_err(|error| {
        if is_unexpected_eof(error) {
            ChunkReadError::SourceChangedBeforeRead
        } else {
            error
        }
    })?;
    if actual != expected {
        return Err(ChunkReadError::CheckpointGuardMismatch);
    }
    Ok(guard_window_len(offset))
}

fn guard_window_len(offset: u64) -> u64 {
    offset.min(GUARD_WINDOW_BYTES)
}

fn compute_guard(file: &mut File, offset: u64) -> Result<Option<GuardHash>, ChunkReadError> {
    if offset == 0 {
        return Ok(None);
    }
    let window_start = offset.saturating_sub(GUARD_WINDOW_BYTES);
    let window_len = usize::try_from(offset - window_start).expect("guard window fits usize");
    let mut window = vec![0_u8; window_len];
    file.seek(SeekFrom::Start(window_start))
        .map_err(|error| io_error(ReadOperation::Seek, error))?;
    file.read_exact(&mut window)
        .map_err(|error| io_error(ReadOperation::Read, error))?;
    Ok(Some(GuardHash(*blake3::hash(&window).as_bytes())))
}

fn read_fixed_range(
    file: &mut File,
    start_offset: u64,
    observed_size: u64,
    requested_bytes: u64,
    on_item: &mut impl FnMut(FramedItem) -> ReadControl,
) -> Result<(ChunkReadResult, bool), ChunkReadError> {
    let mut complete_line_count = 0_u64;
    let mut oversized_complete_line_count = 0_u64;
    let mut line = Vec::new();
    let mut line_start = start_offset;
    let mut line_length = 0_u64;
    let mut oversized = false;
    let mut cursor = start_offset;
    let mut remaining = requested_bytes;
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    let mut peak_buffered_body_bytes = READ_BUFFER_BYTES as u64;
    let mut stopped = false;

    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(READ_BUFFER_BYTES as u64))
            .expect("bounded read size fits usize");
        let count = file
            .read(&mut buffer[..wanted])
            .map_err(|error| io_error(ReadOperation::Read, error))?;
        if count == 0 {
            break;
        }
        remaining -= count as u64;

        for &byte in &buffer[..count] {
            cursor += 1;
            line_length += 1;
            if !oversized {
                if line_length <= MAX_LINE_BYTES {
                    line.push(byte);
                } else {
                    oversized = true;
                    line.clear();
                    line.shrink_to(READ_BUFFER_BYTES);
                }
            }
            peak_buffered_body_bytes =
                peak_buffered_body_bytes.max((READ_BUFFER_BYTES + line.capacity()) as u64);

            if byte == b'\n' {
                let completed_start = line_start;
                let control = if oversized {
                    on_item(FramedItem::OversizedCompleteLine(LineDiagnostic {
                        code: LineDiagnosticCode::OversizedCompleteLine,
                        start_offset: line_start,
                        end_offset: cursor,
                    }))
                } else {
                    on_item(FramedItem::Line(FramedLine {
                        start_offset: line_start,
                        bytes_with_newline: std::mem::take(&mut line),
                    }))
                };
                if control == ReadControl::StopBefore {
                    // The caller inspected the next complete item only to
                    // decide a batch boundary. It is intentionally excluded
                    // from the logical read result and will be reread from
                    // the previous complete boundary in the next batch.
                    cursor = completed_start;
                    stopped = true;
                    line.clear();
                    line_length = 0;
                    oversized = false;
                    break;
                }
                if oversized {
                    oversized_complete_line_count += 1;
                } else {
                    complete_line_count += 1;
                }
                line_start = cursor;
                line_length = 0;
                oversized = false;
                if control == ReadControl::StopAfter {
                    stopped = true;
                    break;
                }
            }
        }
        if stopped {
            break;
        }
    }

    let reached_boundary = cursor == observed_size;
    let has_half_line = reached_boundary && line_start < observed_size;
    Ok((
        ChunkReadResult {
            complete_line_count,
            oversized_complete_line_count,
            last_complete_offset: line_start,
            bytes_read: cursor - start_offset,
            guard_bytes_read: 0,
            peak_buffered_body_bytes,
            has_half_line,
            fixed_view_exhausted: reached_boundary,
            guard: None,
        },
        reached_boundary,
    ))
}

fn modified_ns(metadata: &Metadata) -> i64 {
    file_identity::modified_ns(metadata).unwrap_or(i64::MIN)
}

fn io_error(operation: ReadOperation, error: io::Error) -> ChunkReadError {
    ChunkReadError::Io {
        operation,
        kind: error.kind(),
    }
}

fn is_unexpected_eof(error: ChunkReadError) -> bool {
    matches!(
        error,
        ChunkReadError::Io {
            operation: ReadOperation::Read,
            kind: io::ErrorKind::UnexpectedEof,
        }
    )
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    type CollectedChunk = (ChunkReadResult, Vec<Vec<u8>>, Vec<LineDiagnostic>);

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    mod spec04_p2;

    struct TempFile {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempFile {
        fn new(bytes: &[u8]) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "usagi-chunk-reader-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("rollout-test.jsonl");
            std::fs::write(&path, bytes).unwrap();
            Self { directory, path }
        }

        fn plan(
            &self,
            start_offset: u64,
            observed_size: u64,
            guard: Option<GuardHash>,
        ) -> ChunkReadPlan {
            let file = File::open(&self.path).unwrap();
            ChunkReadPlan {
                path: self.path.clone(),
                identity: file_identity::identity_from_file(&file).unwrap(),
                start_offset,
                observed_size,
                expected_guard: guard,
            }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn collect(plan: &ChunkReadPlan) -> Result<CollectedChunk, ChunkReadError> {
        let mut lines = Vec::new();
        let mut diagnostics = Vec::new();
        let result = read_chunk(plan, |item| match item {
            FramedItem::Line(line) => lines.push(line.json_bytes().to_vec()),
            FramedItem::OversizedCompleteLine(diagnostic) => diagnostics.push(diagnostic),
        })?;
        Ok((result, lines, diagnostics))
    }

    #[test]
    fn fixed_view_does_not_expand_for_an_append_after_discovery() {
        let file = TempFile::new(b"first\n");
        let observed_size = std::fs::metadata(&file.path).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&file.path)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();

        let (result, lines, diagnostics) = collect(&file.plan(0, observed_size, None)).unwrap();

        assert_eq!(result.last_complete_offset, observed_size);
        assert_eq!(result.bytes_read, observed_size);
        assert_eq!(lines, vec![b"first".to_vec()]);
        assert!(diagnostics.is_empty());

        let appended_during_read = TempFile::new(b"first\n");
        let plan = appended_during_read.plan(0, observed_size, None);
        let append_path = appended_during_read.path.clone();
        let mut lines = Vec::new();
        let mut appended = false;
        let result = read_chunk(&plan, |item| {
            if let FramedItem::Line(line) = item {
                lines.push(line.json_bytes().to_vec());
                if !appended {
                    OpenOptions::new()
                        .append(true)
                        .open(&append_path)
                        .unwrap()
                        .write_all(b"second\n")
                        .unwrap();
                    appended = true;
                }
            }
        })
        .unwrap();
        assert_eq!(lines, vec![b"first".to_vec()]);
        assert_eq!(result.bytes_read, observed_size);
        assert_eq!(result.last_complete_offset, observed_size);
    }

    #[test]
    fn guard_matches_an_append_and_rejects_a_seam_rewrite() {
        let file = TempFile::new(b"first\n");
        let (first, _, _) = collect(&file.plan(0, 6, None)).unwrap();
        let guard = first.guard;
        OpenOptions::new()
            .append(true)
            .open(&file.path)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();

        let (_, appended, _) = collect(&file.plan(6, 13, guard)).unwrap();
        assert_eq!(appended, vec![b"second".to_vec()]);

        let mut handle = OpenOptions::new().write(true).open(&file.path).unwrap();
        handle.write_all(b"FIRST").unwrap();
        assert_eq!(
            collect(&file.plan(6, 13, guard)).err(),
            Some(ChunkReadError::CheckpointGuardMismatch)
        );
    }

    #[test]
    fn line_reader_combines_lf_crlf_half_line_and_oversized_cases() {
        let mut bytes = b"lf\ncrlf\r\n\npartial".to_vec();
        let initial_size = bytes.len() as u64;
        let file = TempFile::new(&bytes);
        let (initial, initial_lines, _) = collect(&file.plan(0, initial_size, None)).unwrap();
        assert_eq!(
            initial_lines,
            vec![b"lf".to_vec(), b"crlf".to_vec(), b"".to_vec()]
        );
        assert!(initial.has_half_line);
        assert_eq!(initial.last_complete_offset, 10);

        bytes.extend_from_slice(b"-done\n");
        bytes.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES as usize));
        bytes.extend_from_slice(b"!\n");
        bytes.extend(std::iter::repeat_n(b'y', MAX_LINE_BYTES as usize + 1));
        std::fs::write(&file.path, &bytes).unwrap();

        let completed_size = bytes.len() as u64;
        let (completed, completed_lines, diagnostics) =
            collect(&file.plan(10, completed_size, initial.guard)).unwrap();
        assert_eq!(completed_lines, vec![b"partial-done".to_vec()]);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            LineDiagnosticCode::OversizedCompleteLine
        );
        assert!(completed.has_half_line);
        assert_eq!(completed.last_complete_offset, diagnostics[0].end_offset);
        assert!(completed.guard.is_some());
    }

    #[test]
    fn plan_validation_and_checkpoint_bounds_matrix() {
        let file = TempFile::new(b"line\n");
        let identity = file_identity::identity_from_path(&file.path).unwrap();
        let invalid_guard = Some(GuardHash::from_bytes([7; 32]));

        for (start_offset, observed_size, expected_guard, expected_error) in [
            (0, 5, invalid_guard, ChunkReadError::InvalidGuardPlan),
            (1, 5, None, ChunkReadError::InvalidGuardPlan),
            (6, 5, invalid_guard, ChunkReadError::CheckpointOutOfRange),
            (0, 6, None, ChunkReadError::SourceChangedBeforeRead),
        ] {
            let plan = ChunkReadPlan {
                path: file.path.clone(),
                identity,
                start_offset,
                observed_size,
                expected_guard,
            };
            assert_eq!(collect(&plan).err(), Some(expected_error));
        }

        let (initial, lines, _) = collect(&file.plan(0, 5, None)).unwrap();
        assert_eq!(lines, vec![b"line".to_vec()]);
        assert_eq!(initial.last_complete_offset, 5);
        assert!(initial.last_complete_offset <= 5);

        let (at_boundary, lines, _) = collect(&file.plan(5, 5, initial.guard)).unwrap();
        assert!(lines.is_empty());
        assert_eq!(at_boundary.bytes_read, 0);
        assert_eq!(at_boundary.last_complete_offset, 5);
        assert!(at_boundary.last_complete_offset <= 5);
    }

    #[test]
    fn guard_window_has_an_exact_4096_byte_boundary() {
        let mut bytes = vec![b'a'; 5_000];
        bytes[4_999] = b'\n';
        bytes.extend_from_slice(b"tail\n");
        let file = TempFile::new(&bytes);
        let (initial, _, _) = collect(&file.plan(0, 5_000, None)).unwrap();
        assert_eq!(
            initial.guard,
            Some(GuardHash::from_bytes(
                *blake3::hash(&bytes[904..5_000]).as_bytes()
            ))
        );

        let mut handle = OpenOptions::new().write(true).open(&file.path).unwrap();
        handle.seek(SeekFrom::Start(903)).unwrap();
        handle.write_all(b"b").unwrap();
        drop(handle);
        let (outside_window, lines, _) = collect(&file.plan(5_000, 5_005, initial.guard)).unwrap();
        assert_eq!(lines, vec![b"tail".to_vec()]);
        assert!(outside_window.last_complete_offset <= 5_005);

        let mut handle = OpenOptions::new().write(true).open(&file.path).unwrap();
        handle.seek(SeekFrom::Start(904)).unwrap();
        handle.write_all(b"b").unwrap();
        drop(handle);
        assert_eq!(
            collect(&file.plan(5_000, 5_005, initial.guard)).err(),
            Some(ChunkReadError::CheckpointGuardMismatch)
        );
    }

    #[test]
    fn usage_line_limits_keep_four_to_eight_mib_legal_and_stream_oversized_body() {
        let legal_len = 6 * 1024 * 1024usize;
        let mut bytes = vec![b'a'; legal_len - 1];
        bytes.push(b'\n');
        let oversized_start = bytes.len() as u64;
        bytes.extend(std::iter::repeat_n(b'b', MAX_LINE_BYTES as usize));
        bytes.push(b'\n');
        let observed_size = bytes.len() as u64;
        let file = TempFile::new(&bytes);

        let mut legal_body_len = None;
        let mut diagnostics = Vec::new();
        let result = read_chunk(&file.plan(0, observed_size, None), |item| match item {
            FramedItem::Line(line) => legal_body_len = Some(line.json_bytes().len()),
            FramedItem::OversizedCompleteLine(diagnostic) => diagnostics.push(diagnostic),
        })
        .unwrap();

        assert_eq!(legal_body_len, Some(legal_len - 1));
        assert_eq!(result.complete_line_count, 1);
        assert_eq!(result.oversized_complete_line_count, 1);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].start_offset, oversized_start);
        assert_eq!(diagnostics[0].end_offset, observed_size);
        assert_eq!(result.last_complete_offset, observed_size);
        assert!(!result.has_half_line);
        assert!(result.fixed_view_exhausted);
        assert!(result.peak_buffered_body_bytes <= MAX_BUFFERED_BODY_BYTES);
    }

    #[test]
    fn t_dist_003_replacement_and_truncation_races_never_return_success() {
        let replaced_before_open = TempFile::new(b"one\ntwo\n");
        let before_plan = replaced_before_open.plan(0, 8, None);
        let displaced = replaced_before_open
            .directory
            .join("displaced-before.jsonl");
        std::fs::rename(&replaced_before_open.path, displaced).unwrap();
        std::fs::write(&replaced_before_open.path, b"one\ntwo\n").unwrap();
        assert_eq!(
            collect(&before_plan).err(),
            Some(ChunkReadError::SourceChangedBeforeRead)
        );

        let replaced_after_open = TempFile::new(b"one\ntwo\n");
        let after_plan = replaced_after_open.plan(0, 8, None);
        let replacement_path = replaced_after_open.path.clone();
        let displaced = replaced_after_open.directory.join("displaced-after.jsonl");
        let mut replaced = false;
        assert_eq!(
            read_chunk(&after_plan, |_| {
                if !replaced {
                    std::fs::rename(&replacement_path, &displaced).unwrap();
                    std::fs::write(&replacement_path, b"one\ntwo\n").unwrap();
                    replaced = true;
                }
            })
            .err(),
            Some(ChunkReadError::SourceChangedDuringRead)
        );

        let truncated_during_read = TempFile::new(b"one\ntwo\n");
        let truncation_plan = truncated_during_read.plan(0, 8, None);
        let truncation_path = truncated_during_read.path.clone();
        let mut truncated = false;
        assert_eq!(
            read_chunk(&truncation_plan, |_| {
                if !truncated {
                    OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&truncation_path)
                        .unwrap();
                    truncated = true;
                }
            })
            .err(),
            Some(ChunkReadError::SourceChangedDuringRead)
        );
    }
}
