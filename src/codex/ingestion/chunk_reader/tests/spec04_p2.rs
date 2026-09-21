use super::*;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

fn current_rss_kib() -> u64 {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .expect("query process RSS with ps");
        assert!(output.status.success(), "ps RSS query failed");
        std::str::from_utf8(&output.stdout)
            .expect("ps RSS is UTF-8")
            .trim()
            .parse()
            .expect("ps RSS is numeric KiB")
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::GetCurrentProcess,
        };

        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let result = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        };
        assert_ne!(result, 0, "GetProcessMemoryInfo failed");
        (counters.WorkingSetSize as u64) / 1024
    }
    #[cfg(not(any(unix, windows)))]
    {
        panic!("current process RSS is unsupported on this platform");
    }
}

fn one_gib_valid_json_lines() -> (TempFile, u64, u64) {
    const TARGET_BYTES: u64 = 1024 * 1024 * 1024;
    let line_bytes = crate::codex::ingestion::usage_pipeline::MAX_BATCH_BYTES as usize;
    assert_eq!(TARGET_BYTES % line_bytes as u64, 0);
    let line_count = TARGET_BYTES / line_bytes as u64;

    let prefix = br#"{"type":"response_item","payload":{"type":"resource_probe","padding":""#;
    let suffix = b"\"}}\n";
    let padding_len = line_bytes
        .checked_sub(prefix.len() + suffix.len())
        .expect("4 MiB line leaves room for JSON framing");
    let mut line = Vec::with_capacity(line_bytes);
    line.extend_from_slice(prefix);
    line.extend(std::iter::repeat_n(b'x', padding_len));
    line.extend_from_slice(suffix);
    assert_eq!(line.len(), line_bytes);
    assert_eq!(line.last(), Some(&b'\n'));

    let file = TempFile::new(b"");
    let handle = std::fs::File::create(&file.path).expect("create 1 GiB rollout");
    let mut writer = BufWriter::with_capacity(64 * 1024, handle);
    for _ in 0..line_count {
        writer
            .write_all(&line)
            .expect("write synthetic rollout line");
    }
    writer.flush().expect("flush synthetic rollout");
    drop(writer);
    drop(line);

    let observed_size = std::fs::metadata(&file.path).unwrap().len();
    assert_eq!(observed_size, TARGET_BYTES);
    (file, observed_size, line_count)
}

#[test]
#[ignore = "P2 final resource test: writes and reads an actual 1 GiB rollout"]
fn t_s04_052_one_gib_bounded_reader_keeps_batches_and_process_memory_bounded() {
    const MAX_RSS_GROWTH_KIB: u64 = 128 * 1024;
    const MAX_PROCESS_RSS_KIB: u64 = 384 * 1024;
    const MAX_ELAPSED: Duration = Duration::from_secs(300);

    let fixture_started = Instant::now();
    let (file, observed_size, expected_batches) = one_gib_valid_json_lines();
    let physical_identity =
        crate::platform::file_identity::identity_from_path(&file.path).expect("physical identity");
    let rss_before_read = current_rss_kib();
    let mut sampled_peak_rss = rss_before_read;

    let mut start_offset = 0_u64;
    let mut expected_guard = None;
    let mut batch_count = 0_u64;
    let mut total_body_bytes = 0_u64;
    let mut max_peak_buffered = 0_u64;
    while start_offset < observed_size {
        let batch_start = start_offset;
        let mut delivered_line_bytes = None;
        let result = read_chunk_bounded(
            &ChunkReadPlan {
                path: file.path.clone(),
                identity: physical_identity,
                start_offset,
                observed_size,
                expected_guard,
            },
            |item| match item {
                FramedItem::Line(line) => {
                    delivered_line_bytes = Some(line.into_bytes_with_newline().len() as u64);
                    // Sample while the complete line body is still in this
                    // batch's callback path so the per-batch allocation is
                    // represented in the process peak measurement.
                    sampled_peak_rss = sampled_peak_rss.max(current_rss_kib());
                    ReadControl::StopAfter
                }
                FramedItem::OversizedCompleteLine(_) => {
                    panic!("4 MiB synthetic line must stay within the legal line limit")
                }
            },
        )
        .expect("read one bounded 1 GiB batch");

        let line_bytes = delivered_line_bytes.expect("exactly one complete line per batch");
        assert_eq!(
            line_bytes,
            crate::codex::ingestion::usage_pipeline::MAX_BATCH_BYTES
        );
        assert_eq!(result.complete_line_count, 1);
        assert_eq!(result.oversized_complete_line_count, 0);
        assert_eq!(
            result.bytes_read,
            crate::codex::ingestion::usage_pipeline::MAX_BATCH_BYTES
        );
        assert_eq!(result.last_complete_offset - batch_start, line_bytes);
        assert!(!result.has_half_line);
        assert!(result.peak_buffered_body_bytes <= MAX_BUFFERED_BODY_BYTES);
        assert_eq!(
            result.fixed_view_exhausted,
            batch_count + 1 == expected_batches
        );

        batch_count += 1;
        total_body_bytes = total_body_bytes
            .checked_add(result.bytes_read)
            .expect("resource counter must not overflow");
        max_peak_buffered = max_peak_buffered.max(result.peak_buffered_body_bytes);
        start_offset = result.last_complete_offset;
        expected_guard = result.guard;
    }

    sampled_peak_rss = sampled_peak_rss.max(current_rss_kib());
    let rss_growth = sampled_peak_rss.saturating_sub(rss_before_read);
    let elapsed = fixture_started.elapsed();
    assert_eq!(batch_count, expected_batches);
    assert_eq!(batch_count, 256, "1 GiB / 4 MiB must require 256 batches");
    assert_eq!(total_body_bytes, observed_size);
    assert_eq!(start_offset, observed_size);
    assert!(
        rss_growth <= MAX_RSS_GROWTH_KIB,
        "reader sampled RSS grew by {rss_growth} KiB; batches must not be retained"
    );
    assert!(
        sampled_peak_rss <= MAX_PROCESS_RSS_KIB,
        "reader sampled RSS {sampled_peak_rss} KiB exceeded the explicit P2 budget"
    );
    assert!(
        elapsed <= MAX_ELAPSED,
        "1 GiB fixture+read took {elapsed:?}, exceeding the explicit P2 budget"
    );
}
