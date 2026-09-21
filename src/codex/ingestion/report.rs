//! Bounded, privacy-safe counters for one metadata scan round.
//!
//! A report contains only counts, timestamps, source IDs, and fixed error
//! codes.  It never stores rollout rows, JSON, or parser payloads.

use std::time::Duration;

use crate::codex::domain::{BuildDisposition, SourceObservationResult};
use crate::codex::storage::usage::UsageCommitOutcome;

use super::chunk_reader::ChunkReadResult;
use super::usage_consumer::UsageCommitMetrics;

use super::metadata_pipeline::{FilePlan, ParsedSource, PipelinePlan};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScanReport {
    pub(crate) started_at_ms: i64,
    pub(crate) elapsed_ms: i64,
    pub(crate) discovered_files: u64,
    pub(crate) added_sources: u64,
    pub(crate) moved_sources: u64,
    pub(crate) appended_sources: u64,
    pub(crate) rebuilt_sources: u64,
    pub(crate) skipped_sources: u64,
    pub(crate) successful_sources: u64,
    pub(crate) failed_sources: u64,
    pub(crate) bytes_read: u64,
    pub(crate) guard_bytes_read: u64,
    pub(crate) body_open_attempts: u64,
    pub(crate) peak_buffered_body_bytes: u64,
    pub(crate) complete_lines: u64,
    pub(crate) oversized_complete_lines: u64,
    pub(crate) half_lines: u64,
    pub(crate) malformed_records: u64,
    pub(crate) diagnostics: u64,
    pub(crate) usage_lines_seen: u64,
    pub(crate) token_records_seen: u64,
    pub(crate) usage_events_inserted: u64,
    pub(crate) usage_events_deduplicated: u64,
    pub(crate) normal_events: u64,
    pub(crate) recovered_events: u64,
    pub(crate) compensation_events: u64,
    pub(crate) anomalies_created: u64,
    pub(crate) usage_bytes_read: u64,
    pub(crate) usage_parse_duration_ms: u64,
    pub(crate) usage_worklist_loads: u64,
    pub(crate) usage_worklist_candidates: u64,
    pub(crate) usage_detail_plan_loads: u64,
    pub(crate) usage_detail_sources_loaded: u64,
    pub(crate) usage_global_replans: u64,
    pub(crate) usage_worklist_duration_ms: u64,
    pub(crate) usage_detail_plan_duration_ms: u64,
    pub(crate) usage_detail_source_ids: Vec<i64>,
    pub(crate) usage_db_write_duration_ms: u64,
    pub(crate) source_file_ids: Vec<i64>,
    pub(crate) error_codes: Vec<&'static str>,
}

impl ScanReport {
    pub(crate) fn new(started_at_ms: i64) -> Self {
        Self {
            started_at_ms: started_at_ms.max(0),
            ..Self::default()
        }
    }

    pub(crate) fn observe_discovery(&mut self, count: usize) {
        self.discovered_files = self.discovered_files.saturating_add(count as u64);
    }

    pub(crate) fn observe_source(&mut self, result: &SourceObservationResult) {
        self.source_file_ids.push(result.source_file_id);
        if result.created {
            self.added_sources = self.added_sources.saturating_add(1);
        }
        if result.moved {
            self.moved_sources = self.moved_sources.saturating_add(1);
        }
        if result.replaced
            || result
                .rebuild_consumers
                .contains(&crate::codex::domain::ConsumerKind::Metadata)
        {
            self.rebuilt_sources = self.rebuilt_sources.saturating_add(1);
        }
        if result.build_disposition == BuildDisposition::CarryResumedPresent {
            self.appended_sources = self.appended_sources.saturating_add(1);
        }
    }

    pub(crate) fn observe_plan(&mut self, plan: &PipelinePlan) {
        for file_plan in &plan.plans {
            if matches!(file_plan, FilePlan::Skip { .. }) {
                self.skipped_sources = self.skipped_sources.saturating_add(1);
                self.successful_sources = self.successful_sources.saturating_add(1);
            }
        }
    }

    pub(crate) fn observe_parse(&mut self, parsed: &ParsedSource) {
        self.bytes_read = self.bytes_read.saturating_add(parsed.bytes_read);
        self.guard_bytes_read = self
            .guard_bytes_read
            .saturating_add(parsed.guard_bytes_read);
        self.peak_buffered_body_bytes = self
            .peak_buffered_body_bytes
            .max(parsed.peak_buffered_body_bytes);
        self.complete_lines = self
            .complete_lines
            .saturating_add(parsed.complete_line_count);
        self.oversized_complete_lines = self
            .oversized_complete_lines
            .saturating_add(parsed.oversized_complete_line_count);
        if parsed.has_half_line {
            self.half_lines = self.half_lines.saturating_add(1);
        }
        self.malformed_records = self
            .malformed_records
            .saturating_add(parsed.malformed_record_count);
        self.diagnostics = self.diagnostics.saturating_add(parsed.diagnostic_count);
        if parsed.oversized_complete_line_count > 0 {
            self.error("OVERSIZED_COMPLETE_LINE");
        }
        if parsed.malformed_record_count > 0 {
            self.error("MALFORMED_RECORD");
        }
        if parsed.stable() {
            self.successful_sources = self.successful_sources.saturating_add(1);
        }
    }

    pub(crate) fn observe_usage_read(
        &mut self,
        read: &ChunkReadResult,
        token_records_seen: u64,
        elapsed: Duration,
    ) {
        self.usage_lines_seen = self
            .usage_lines_seen
            .saturating_add(read.complete_line_count)
            .saturating_add(read.oversized_complete_line_count);
        self.token_records_seen = self.token_records_seen.saturating_add(token_records_seen);
        self.usage_bytes_read = self.usage_bytes_read.saturating_add(read.bytes_read);
        self.usage_parse_duration_ms = self
            .usage_parse_duration_ms
            .saturating_add(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
        self.peak_buffered_body_bytes = self
            .peak_buffered_body_bytes
            .max(read.peak_buffered_body_bytes);
    }

    pub(crate) fn observe_usage_worklist_load(&mut self, candidates: usize, elapsed: Duration) {
        self.usage_worklist_loads = self.usage_worklist_loads.saturating_add(1);
        self.usage_worklist_candidates = self
            .usage_worklist_candidates
            .saturating_add(candidates as u64);
        self.usage_worklist_duration_ms = self
            .usage_worklist_duration_ms
            .saturating_add(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
    }

    pub(crate) fn observe_usage_detail_plan_load(&mut self, source_ids: &[i64], elapsed: Duration) {
        self.usage_detail_plan_loads = self.usage_detail_plan_loads.saturating_add(1);
        self.usage_detail_sources_loaded = self
            .usage_detail_sources_loaded
            .saturating_add(source_ids.len() as u64);
        self.usage_detail_plan_duration_ms = self
            .usage_detail_plan_duration_ms
            .saturating_add(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
        self.usage_detail_source_ids.extend_from_slice(source_ids);
    }

    pub(crate) fn observe_usage_global_replan(&mut self) {
        self.usage_global_replans = self.usage_global_replans.saturating_add(1);
    }

    pub(crate) fn observe_usage_commit(
        &mut self,
        metrics: &UsageCommitMetrics,
        outcome: &UsageCommitOutcome,
        elapsed: Duration,
    ) {
        self.usage_events_inserted = self
            .usage_events_inserted
            .saturating_add(outcome.events_inserted as u64);
        self.usage_events_deduplicated = self
            .usage_events_deduplicated
            .saturating_add(outcome.events_deduplicated as u64);
        self.normal_events = self.normal_events.saturating_add(metrics.normal_events);
        self.recovered_events = self
            .recovered_events
            .saturating_add(metrics.recovered_events);
        self.compensation_events = self
            .compensation_events
            .saturating_add(metrics.compensation_events);
        self.anomalies_created = self.anomalies_created.saturating_add(metrics.anomalies);
        self.usage_db_write_duration_ms = self
            .usage_db_write_duration_ms
            .saturating_add(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
    }

    pub(crate) fn observe_body_open_attempt(&mut self) {
        self.body_open_attempts = self.body_open_attempts.saturating_add(1);
    }

    pub(crate) fn failed_source(&mut self) {
        self.failed_sources = self.failed_sources.saturating_add(1);
    }

    pub(crate) fn error(&mut self, code: &'static str) {
        if !self.error_codes.contains(&code) {
            self.error_codes.push(code);
        }
    }

    pub(crate) fn finish(&mut self, finished_at_ms: i64) {
        self.elapsed_ms = finished_at_ms.max(0).saturating_sub(self.started_at_ms);
        self.source_file_ids.sort_unstable();
        self.source_file_ids.dedup();
        self.usage_detail_source_ids.sort_unstable();
        self.usage_detail_source_ids.dedup();
        self.error_codes.sort_unstable();

        // Keep every report field exercised while retaining the report as a
        // bounded, internal value until a future status/report consumer reads
        // it.  No user payload is retained here.
        let _ = (
            self.started_at_ms,
            self.elapsed_ms,
            self.discovered_files,
            self.added_sources,
            self.moved_sources,
            self.appended_sources,
            self.rebuilt_sources,
            self.skipped_sources,
            self.successful_sources,
            self.failed_sources,
            self.bytes_read,
            self.guard_bytes_read,
            self.body_open_attempts,
            self.peak_buffered_body_bytes,
            self.complete_lines,
            self.oversized_complete_lines,
            self.half_lines,
            self.malformed_records,
            self.diagnostics,
            self.usage_lines_seen,
            self.token_records_seen,
            self.usage_events_inserted,
            self.usage_events_deduplicated,
            self.normal_events,
            self.recovered_events,
            self.compensation_events,
            self.anomalies_created,
            self.usage_bytes_read,
            self.usage_parse_duration_ms,
            self.usage_worklist_loads,
            self.usage_worklist_candidates,
            self.usage_detail_plan_loads,
            self.usage_detail_sources_loaded,
            self.usage_global_replans,
            self.usage_worklist_duration_ms,
            self.usage_detail_plan_duration_ms,
            &self.usage_detail_source_ids,
            self.usage_db_write_duration_ms,
            &self.source_file_ids,
            &self.error_codes,
        );
    }
}
