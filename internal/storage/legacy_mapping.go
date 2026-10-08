package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
)

// These literal column sets are the Current targets from the frozen schema.
var currentImportColumns = map[string][]string{
	"antigravity_conversation_state":         {"conversation_id", "last_scanned_at_ms", "observed_gen_max_idx", "observed_step_max_idx"},
	"antigravity_usage_quarantine":           {"conversation_id", "first_seen_at_ms", "gen_idx", "last_seen_at_ms", "payload_digest", "reason_code", "response_id"},
	"app_meta":                               {"active_scan_id", "cost_algorithm_version", "data_revision", "followup_enqueued_status_revision", "followup_error_code", "followup_requested_at_ms", "followup_scan_id", "followup_state", "followup_trigger", "id", "last_finished_scan_id", "last_finished_scan_result", "last_scan_completed_at_ms", "last_scan_error_code", "last_scan_failed_at_ms", "last_scan_started_at_ms", "pricing_catalog_version", "scan_state", "status_revision"},
	"codex_adapter_state":                    {"binding_status", "home_fingerprint", "id"},
	"codex_compaction_markers":               {"file_generation", "ledger_epoch", "model", "occurred_at_ms", "owning_thread_id", "reasoning_effort", "resolved_event_id", "response_id", "root_session_id", "source", "source_end_offset", "source_file_id", "source_start_offset", "unknown_reason"},
	"codex_ingest_anomalies":                 {"anomaly_id", "anomaly_type", "details_json", "detected_at_ms", "file_generation", "ledger_epoch", "occurred_at_ms", "resolved", "severity", "source_file_id", "source_start_offset", "thread_id"},
	"codex_rollout_metadata_facts":           {"agent_path", "agent_path_provenance", "agent_path_record_offset", "agent_role_hint", "agent_role_provenance", "agent_role_record_offset", "continuation_state", "created_at_ms", "cwd", "cwd_provenance", "cwd_record_offset", "fact_quality_status", "file_generation", "latest_context_at_ms", "latest_context_model", "latest_context_turn_id", "metadata_parser_version", "ownership_confidence", "owning_records_start_offset", "owning_thread_id", "parent_hint_provenance", "parent_hint_record_offset", "parent_thread_id_hint", "relationship_conflict", "replay_start_offset", "resolved_through_offset", "source_file_id", "updated_at_ms"},
	"codex_skill_usage_events":               {"created_at_ms", "file_generation", "ledger_epoch", "model", "occurred_at_ms", "root_session_id", "skill_name", "source_end_offset", "source_file_id", "source_start_offset", "thread_id"},
	"codex_source_checkpoints":               {"committed_offset", "consumer_kind", "guard_hash", "last_error_code", "last_successful_scan_at_ms", "parser_version", "processing_status", "source_file_id"},
	"codex_source_files":                     {"current_path", "device_id", "file_generation", "file_status", "inode", "last_seen_at_ms", "observed_mtime_ns", "observed_size", "source_area", "source_file_id", "thread_id"},
	"codex_turns":                            {"accounted_cache_write_tokens", "accounted_cached_tokens", "accounted_candidate_count", "accounted_fingerprint", "accounted_input_tokens", "accounted_output_tokens", "accounted_reasoning_tokens", "accounted_total_tokens", "block_model_unresolved", "block_ownership_gap", "block_parser_gap", "block_required_invalid", "block_reset", "block_start_missing", "block_time_missing", "compensation_allowed", "end_offset", "ended_at_ms", "file_generation", "last_total_cache_write_tokens", "last_total_cached_tokens", "last_total_fingerprint", "last_total_input_tokens", "last_total_output_tokens", "last_total_reasoning_tokens", "last_total_total_tokens", "ledger_epoch", "model_state", "quality_status", "raw_turn_id", "reasoning_effort_state", "single_model", "single_reasoning_effort", "source_file_id", "start_offset", "start_total_cache_write_tokens", "start_total_cached_tokens", "start_total_fingerprint", "start_total_input_tokens", "start_total_output_tokens", "start_total_reasoning_tokens", "start_total_total_tokens", "started_at_ms", "state_through_offset", "status", "thread_id", "turn_key", "unresolved_model_seen", "unresolved_reasoning_effort_seen", "updated_at_ms"},
	"codex_usage_build_sources":              {"active_committed_offset", "active_guard_hash", "active_state_fingerprint", "build_epoch", "carry_after_anomaly_id", "carry_after_fact_event_id", "carry_after_marker_start_offset", "carry_after_start_offset", "carry_after_turn_key", "carry_after_window_start_offset", "carry_from_epoch", "carry_phase", "completed_generation", "completed_through_offset", "completion_error_code", "completion_status", "created_at_ms", "expected_device_id", "expected_file_generation", "expected_inode", "expected_owning_thread_id", "expected_root_session_id", "membership_reason", "observed_raw_size", "raw_tail_start_offset", "raw_tail_status", "required_generation", "required_through_offset", "source_file_id", "target_parser_version", "updated_at_ms"},
	"codex_usage_event_facts":                {"event_id", "evidence_kind", "ledger_epoch", "operation", "owning_thread_id", "response_id", "source"},
	"codex_usage_event_holds":                {"event_id", "file_generation", "hold_reason", "ledger_epoch", "source", "source_file_id"},
	"codex_usage_event_occurrences":          {"created_at_ms", "event_id", "file_generation", "ledger_epoch", "source", "source_end_offset", "source_file_id", "source_start_offset"},
	"codex_usage_reconciliation_windows":     {"file_generation", "ledger_epoch", "owning_thread_id", "source", "source_end_offset", "source_file_id", "source_start_offset", "state_json", "turn_key"},
	"codex_usage_session_quarantine":         {"first_seen_at_ms", "last_activity_at_ms", "ledger_epoch", "primary_error_code", "root_session_id", "updated_at_ms"},
	"codex_usage_session_quarantine_sources": {"device_id", "file_generation", "inode", "ledger_epoch", "observed_size", "root_session_id", "source_file_id", "updated_at_ms"},
	"codex_usage_source_states":              {"active_model", "active_model_offset", "active_reasoning_effort", "active_reasoning_effort_offset", "active_turn_key", "canonical_algorithm_version", "chain_block_reason", "chain_state", "continuation_state", "device_id", "file_generation", "inode", "ledger_epoch", "observed_raw_size", "owning_thread_id", "previous_total_cache_write_tokens", "previous_total_cached_tokens", "previous_total_fingerprint", "previous_total_input_tokens", "previous_total_offset", "previous_total_output_tokens", "previous_total_reasoning_tokens", "previous_total_total_tokens", "raw_tail_start_offset", "raw_tail_status", "reconciliation_state_json", "resolved_through_offset", "root_session_id", "source_file_id", "updated_at_ms", "usage_parser_version"},
	"scan_runs":                              {"enqueued_status_revision", "error_code", "finished_at_ms", "request_kind", "requested_at_ms", "scan_id", "started_at_ms", "started_status_revision", "state", "terminal_status_revision", "trigger"},
	"source_scan_runs":                       {"error_code", "finished_at_ms", "scan_id", "source", "started_at_ms", "state"},
	"source_usage_epochs":                    {"active_epoch", "active_parser_version", "build_epoch", "build_parser_version", "source"},
	"threads":                                {"agent_role", "archived", "created_at_ms", "metadata_model", "metadata_quality_status", "metadata_resolved_at_ms", "native_session_id", "parent_thread_id", "project_kind", "project_name", "project_path", "root_session_id", "source", "thread_id", "title", "updated_at_ms"},
	"usage_events":                           {"cache_write_tokens", "cached_tokens", "created_at_ms", "estimated_cost_nanos_usd", "event_id", "event_kind", "input_tokens", "model", "occurred_at_ms", "output_tokens", "quality_status", "reasoning_effort", "reasoning_tokens", "root_session_id", "source", "source_epoch", "thread_id", "total_tokens", "turn_key"},
}

const legacyReconciliationState = `{"version":1,"open_window_start_offset":null,"pending_response_ids":[],"modern_counter_domain":null,"modern_counter_total":null,"pending_evidence":[]}`

type legacyImporter struct {
	ctx      context.Context
	version  int
	variant  string
	source   *sql.Tx
	target   *sql.Tx
	profiles map[int]legacyProfile
}

func importLegacy(ctx context.Context, sourceVersion int, variant string, srcTx, dstTx *sql.Tx) error {
	for _, statement := range []string{
		"DELETE FROM app_meta WHERE id = 1",
		"DELETE FROM codex_adapter_state WHERE id = 1",
		"DELETE FROM source_usage_epochs WHERE source = 'codex'",
	} {
		if _, err := dstTx.ExecContext(ctx, statement); err != nil {
			return mapSQLiteError(err)
		}
	}
	profiles, err := loadLegacyProfiles()
	if err != nil {
		return err
	}
	importer := legacyImporter{ctx: ctx, version: sourceVersion, variant: variant, source: srcTx, target: dstTx, profiles: profiles}
	// The calls follow the eight frozen dependency layers in 4.2.8.
	for _, importTable := range []func() error{
		importer.importAppMeta, importer.importEpochs, importer.importAdapter,
		importer.importThreads, importer.importScanRuns, importer.importSourceScanRuns,
		importer.importSourceFiles, importer.importCheckpoints, importer.importMetadataFacts,
		importer.importUsageEvents, importer.importOccurrences, importer.importEventFacts,
		importer.importCompactionMarkers, importer.importReconciliationWindows, importer.importEventHolds,
		importer.importTurns, importer.importAnomalies, importer.importSourceStates, importer.importBuildSources,
		importer.importQuarantine, importer.importQuarantineSources, importer.importSkillEvents,
		importer.importAntigravityState, importer.importAntigravityQuarantine,
	} {
		if err := importTable(); err != nil {
			return err
		}
	}
	return nil
}

func (i *legacyImporter) oldName(current, old string) string {
	if i.version < 12 {
		return old
	}
	return current
}

func (i *legacyImporter) copy(target, source string, defaults map[string]any, renames map[string]string, derive func(map[string]any)) error {
	columns := i.profiles[i.version].Tables[source]
	if columns == nil {
		return newStorageError(ErrorInvalidState, fmt.Errorf("no source rule for %s at v%d", source, i.version))
	}
	if source == "app_meta" && i.variant != "" {
		selected := make([]string, 0, len(columns))
		for _, column := range columns {
			if column == "metadata_parser_version" && (i.variant == "app_meta_without_metadata_parser_version" || i.variant == "app_meta_without_both_v11_assist_columns") {
				continue
			}
			if column == "last_full_import_completed_at_ms" && (i.variant == "app_meta_without_last_full_import_completed_at_ms" || i.variant == "app_meta_without_both_v11_assist_columns") {
				continue
			}
			selected = append(selected, column)
		}
		columns = selected
	}
	return copyNamedColumns(i.ctx, i.source, i.target, source, target, columns, currentImportColumns[target], defaults, renames, derive)
}

func copyNamedColumns(ctx context.Context, srcTx, dstTx *sql.Tx, source, target string, sourceColumns, targetColumns []string, defaults map[string]any, renames map[string]string, derive func(map[string]any)) error {
	identifiers := make([]string, len(sourceColumns))
	for n, column := range sourceColumns {
		identifiers[n] = quoteIdentifier(column)
	}
	query := "SELECT " + strings.Join(identifiers, ",") + " FROM " + quoteIdentifier(source)
	return copyLegacyRows(ctx, srcTx, dstTx, source, target, query, sourceColumns, targetColumns, defaults, renames, derive)
}

func copyLegacyRows(ctx context.Context, srcTx, dstTx *sql.Tx, source, target, query string, sourceColumns, targetColumns []string, defaults map[string]any, renames map[string]string, derive func(map[string]any)) error {
	rows, err := srcTx.QueryContext(ctx, query)
	if err != nil {
		return mapSQLiteError(err)
	}
	defer rows.Close()
	targetIdentifiers := make([]string, len(targetColumns))
	placeholders := make([]string, len(targetColumns))
	for n, column := range targetColumns {
		targetIdentifiers[n] = quoteIdentifier(column)
		placeholders[n] = "?"
	}
	insertPrefix := "INSERT INTO " + quoteIdentifier(target) + " (" + strings.Join(targetIdentifiers, ",") + ") VALUES ("
	for rows.Next() {
		values := make([]any, len(sourceColumns))
		pointers := make([]any, len(values))
		for n := range values {
			pointers[n] = &values[n]
		}
		if err := rows.Scan(pointers...); err != nil {
			return mapSQLiteError(err)
		}
		row := make(map[string]any, len(values))
		for n, column := range sourceColumns {
			switch values[n].(type) {
			case nil, int64, string, []byte:
			default:
				return newStorageError(ErrorInvalidState, fmt.Errorf("unsupported SQLite value in %s.%s", source, column))
			}
			row[column] = values[n]
		}
		if source == "app_meta" {
			if _, present := row["metadata_parser_version"]; !present {
				row["metadata_parser_version"] = int64(0)
			}
			if _, present := row["last_full_import_completed_at_ms"]; !present {
				row["last_full_import_completed_at_ms"] = nil
			}
		}
		if derive != nil {
			derive(row)
		}
		targetValues := make([]any, len(targetColumns))
		for n, column := range targetColumns {
			if value, present := defaults[column]; present {
				targetValues[n] = value
				continue
			}
			sourceColumn := column
			if renamed, present := renames[column]; present {
				sourceColumn = renamed
			}
			value, present := row[sourceColumn]
			if !present {
				return newStorageError(ErrorInvalidState, fmt.Errorf("unmapped Current column %s.%s", target, column))
			}
			targetValues[n] = value
		}
		for n, value := range targetValues {
			placeholders[n] = "?"
			// The driver reads an empty BLOB as nil []byte, then binds it as NULL.
			if blob, ok := value.([]byte); ok && len(blob) == 0 {
				placeholders[n] = "zeroblob(?)"
				targetValues[n] = int64(0)
			}
		}
		insert := insertPrefix + strings.Join(placeholders, ",") + ")"
		if _, err := dstTx.ExecContext(ctx, insert, targetValues...); err != nil {
			return mapSQLiteError(err)
		}
	}
	return mapSQLiteError(rows.Err())
}

func (i *legacyImporter) importAppMeta() error {
	defaults := map[string]any{}
	if i.version < 7 {
		defaults["cost_algorithm_version"] = int64(0)
		defaults["pricing_catalog_version"] = int64(0)
	}
	return i.copy("app_meta", "app_meta", defaults, nil, nil)
}
func (i *legacyImporter) importEpochs() error {
	if i.version >= 11 {
		return i.copy("source_usage_epochs", "source_usage_epochs", nil, nil, nil)
	}
	if i.version == 1 {
		return i.copy("source_usage_epochs", "app_meta", map[string]any{"source": "codex", "active_epoch": int64(0), "build_epoch": nil, "active_parser_version": int64(0), "build_parser_version": nil}, nil, nil)
	}
	return i.copy("source_usage_epochs", "app_meta", map[string]any{"source": "codex"}, map[string]string{"active_epoch": "usage_active_epoch", "build_epoch": "usage_build_epoch", "active_parser_version": "usage_parser_version", "build_parser_version": "usage_build_parser_version"}, nil)
}
func (i *legacyImporter) importAdapter() error {
	if i.version >= 12 {
		return i.copy("codex_adapter_state", "codex_adapter_state", nil, nil, nil)
	}
	return i.copy("codex_adapter_state", "app_meta", map[string]any{"id": int64(1)}, map[string]string{"home_fingerprint": "codex_home_fingerprint", "binding_status": "source_binding_status"}, nil)
}
func (i *legacyImporter) importThreads() error {
	if i.version >= 11 {
		return i.copy("threads", "threads", nil, nil, nil)
	}
	if i.version >= 5 {
		return i.copy("threads", "threads", map[string]any{"source": "codex"}, map[string]string{"native_session_id": "thread_id"}, nil)
	}
	columns := append([]string(nil), i.profiles[i.version].Tables["threads"]...)
	expressions := make([]string, len(columns))
	for n, column := range columns {
		expressions[n] = quoteIdentifier(column)
	}
	expressions = append(expressions, "CASE WHEN project_path IS NOT NULL AND length(project_path)>0 THEN 'project' ELSE 'unknown' END")
	columns = append(columns, "project_kind")
	query := "SELECT " + strings.Join(expressions, ",") + " FROM threads"
	return copyLegacyRows(i.ctx, i.source, i.target, "threads", "threads", query, columns, currentImportColumns["threads"], map[string]any{"source": "codex"}, map[string]string{"native_session_id": "thread_id"}, nil)
}
func (i *legacyImporter) importScanRuns() error {
	return i.copy("scan_runs", "scan_runs", nil, nil, nil)
}
func (i *legacyImporter) importSourceScanRuns() error {
	if i.version < 11 {
		return nil
	}
	return i.copy("source_scan_runs", "source_scan_runs", nil, nil, nil)
}
func (i *legacyImporter) importSourceFiles() error {
	return i.copy("codex_source_files", i.oldName("codex_source_files", "source_files"), nil, nil, nil)
}
func (i *legacyImporter) importCheckpoints() error {
	return i.copy("codex_source_checkpoints", i.oldName("codex_source_checkpoints", "source_checkpoints"), nil, nil, nil)
}
func (i *legacyImporter) importMetadataFacts() error {
	defaults := map[string]any{}
	if i.version < 6 {
		defaults["agent_path"] = nil
		defaults["agent_path_provenance"] = nil
		defaults["agent_path_record_offset"] = nil
	}
	if i.version < 10 {
		defaults["latest_context_turn_id"] = nil
		defaults["relationship_conflict"] = int64(0)
	}
	return i.copy("codex_rollout_metadata_facts", i.oldName("codex_rollout_metadata_facts", "rollout_metadata_facts"), defaults, nil, nil)
}
func (i *legacyImporter) importUsageEvents() error {
	if i.version == 1 {
		return nil
	}
	defaults := map[string]any{}
	renames := map[string]string{}
	if i.version < 11 {
		defaults["source"] = "codex"
		renames["source_epoch"] = "ledger_epoch"
	}
	if i.version < 7 {
		defaults["reasoning_effort"] = nil
		defaults["estimated_cost_nanos_usd"] = nil
	}
	if i.version == 2 {
		renames["cached_tokens"] = "cached_input_tokens"
		renames["reasoning_tokens"] = "reasoning_output_tokens"
	}
	return i.copy("usage_events", "usage_events", defaults, renames, func(row map[string]any) {
		if i.version == 2 {
			row["cache_write_tokens"] = row["cache_write_input_tokens"]
			if row["cache_write_status"] == "unknown_missing" {
				row["cache_write_tokens"] = nil
			}
		}
	})
}
func (i *legacyImporter) importOccurrences() error {
	if i.version == 1 {
		return nil
	}
	defaults := map[string]any{}
	if i.version < 11 {
		defaults["source"] = "codex"
	}
	if err := i.copy("codex_usage_event_occurrences", i.oldName("codex_usage_event_occurrences", "usage_event_occurrences"), defaults, nil, nil); err != nil {
		return err
	}
	if i.version >= 11 {
		return nil
	}
	rows, err := i.source.QueryContext(i.ctx, "SELECT ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms FROM usage_events")
	if err != nil {
		return mapSQLiteError(err)
	}
	defer rows.Close()
	for rows.Next() {
		var epoch, file, generation, start, end, created int64
		var event string
		if err := rows.Scan(&epoch, &file, &generation, &start, &end, &event, &created); err != nil {
			return mapSQLiteError(err)
		}
		var existing string
		err := i.target.QueryRowContext(i.ctx, "SELECT event_id FROM codex_usage_event_occurrences WHERE source=? AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND source_start_offset=?", "codex", epoch, file, generation, start).Scan(&existing)
		if err == nil {
			if existing != event {
				return newStorageError(ErrorInvalidState, fmt.Errorf("Occurrence physical location belongs to %s, not %s", existing, event))
			}
			continue
		}
		if !errors.Is(err, sql.ErrNoRows) {
			return mapSQLiteError(err)
		}
		if _, err := i.target.ExecContext(i.ctx, "INSERT INTO codex_usage_event_occurrences(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms) VALUES (?,?,?,?,?,?,?,?)", "codex", epoch, file, generation, start, end, event, created); err != nil {
			return mapSQLiteError(err)
		}
	}
	return mapSQLiteError(rows.Err())
}
func (i *legacyImporter) importEventFacts() error {
	if i.version < 14 {
		return nil
	}
	return i.copy("codex_usage_event_facts", "codex_usage_event_facts", nil, nil, nil)
}
func (i *legacyImporter) importCompactionMarkers() error {
	if i.version < 14 {
		return nil
	}
	return i.copy("codex_compaction_markers", "codex_compaction_markers", nil, nil, nil)
}
func (i *legacyImporter) importReconciliationWindows() error {
	if i.version < 14 {
		return nil
	}
	return i.copy("codex_usage_reconciliation_windows", "codex_usage_reconciliation_windows", nil, nil, nil)
}
func (i *legacyImporter) importEventHolds() error {
	if i.version < 14 {
		return nil
	}
	return i.copy("codex_usage_event_holds", "codex_usage_event_holds", nil, nil, nil)
}

func normalizedLegacyTokens(row map[string]any, prefixes []string) {
	for _, prefix := range prefixes {
		row[prefix+"cached_tokens"] = row[prefix+"cached_input_tokens"]
		row[prefix+"reasoning_tokens"] = row[prefix+"reasoning_output_tokens"]
		row[prefix+"total_tokens"] = row[prefix+"derived_total_tokens"]
		row[prefix+"cache_write_tokens"] = row[prefix+"cache_write_input_tokens"]
		if row[prefix+"cache_write_status"] == "unknown_missing" {
			row[prefix+"cache_write_tokens"] = nil
		}
	}
}
func (i *legacyImporter) importTurns() error {
	if i.version == 1 {
		return nil
	}
	defaults := map[string]any{}
	if i.version < 7 {
		defaults["reasoning_effort_state"] = "none"
		defaults["single_reasoning_effort"] = nil
		defaults["unresolved_reasoning_effort_seen"] = int64(0)
	}
	return i.copy("codex_turns", i.oldName("codex_turns", "turns"), defaults, nil, func(row map[string]any) {
		if i.version == 2 {
			normalizedLegacyTokens(row, []string{"start_total_", "last_total_", "accounted_"})
		}
	})
}
func (i *legacyImporter) importAnomalies() error {
	if i.version == 1 {
		return nil
	}
	return i.copy("codex_ingest_anomalies", i.oldName("codex_ingest_anomalies", "ingest_anomalies"), nil, nil, nil)
}
func (i *legacyImporter) importSourceStates() error {
	if i.version == 1 {
		return nil
	}
	defaults := map[string]any{}
	if i.version < 7 {
		defaults["active_reasoning_effort"] = nil
		defaults["active_reasoning_effort_offset"] = nil
	}
	if i.version < 14 {
		defaults["reconciliation_state_json"] = legacyReconciliationState
	}
	return i.copy("codex_usage_source_states", i.oldName("codex_usage_source_states", "usage_source_states"), defaults, nil, func(row map[string]any) {
		if i.version == 2 {
			normalizedLegacyTokens(row, []string{"previous_total_"})
		}
	})
}
func (i *legacyImporter) importBuildSources() error {
	if i.version == 1 {
		return nil
	}
	defaults := map[string]any{}
	if i.version < 14 {
		defaults["carry_after_fact_event_id"] = nil
		defaults["carry_after_marker_start_offset"] = nil
		defaults["carry_after_window_start_offset"] = nil
	}
	return i.copy("codex_usage_build_sources", i.oldName("codex_usage_build_sources", "usage_build_sources"), defaults, nil, nil)
}
func (i *legacyImporter) importQuarantine() error {
	if i.version < 8 {
		return nil
	}
	return i.copy("codex_usage_session_quarantine", i.oldName("codex_usage_session_quarantine", "usage_session_quarantine"), nil, nil, nil)
}
func (i *legacyImporter) importQuarantineSources() error {
	if i.version < 8 {
		return nil
	}
	return i.copy("codex_usage_session_quarantine_sources", i.oldName("codex_usage_session_quarantine_sources", "usage_session_quarantine_sources"), nil, nil, nil)
}
func (i *legacyImporter) importSkillEvents() error {
	if i.version < 9 {
		return nil
	}
	return i.copy("codex_skill_usage_events", i.oldName("codex_skill_usage_events", "skill_usage_events"), nil, nil, nil)
}
func (i *legacyImporter) importAntigravityState() error {
	if i.version < 13 {
		return nil
	}
	return i.copy("antigravity_conversation_state", "antigravity_conversation_state", nil, nil, nil)
}
func (i *legacyImporter) importAntigravityQuarantine() error {
	if i.version < 13 {
		return nil
	}
	return i.copy("antigravity_usage_quarantine", "antigravity_usage_quarantine", nil, nil, nil)
}
