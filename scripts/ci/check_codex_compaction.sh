#!/usr/bin/env bash
set -uo pipefail

SCRIPT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
ROOT_DIR="$(cd "$(dirname "$SCRIPT_PATH")/../.." && pwd)"

architecture_residue_check() {
        local failed=0
        local parser_route
        local normalized_record
        local canonical_schema
        local token_totals
        local token_usage_dto
        local main_model_dto
        local subagent_model_dto
        local add_assign
        local dto
        local file
        local matches
        local rg_status
        local runtime_files=(
            src/codex/normalization.rs
            src/codex/ingestion/usage_pipeline.rs
            src/codex/ingestion/usage_processor.rs
            src/codex/ingestion/usage_commit.rs
            src/codex/ingestion/usage_consumer.rs
            src/codex/storage/rebuild.rs
            src/codex/storage/usage.rs
        )
        local public_files=(
            src/api.rs
            src/api/query.rs
            src/usage/aggregate.rs
            src/usage/ledger.rs
            src/usage/mod.rs
        )
        local private_tables=(
            codex_usage_event_facts
            codex_compaction_markers
            codex_usage_reconciliation_windows
            codex_usage_event_holds
        )
        local private_columns=(
            evidence_kind
            operation
            response_id
            compaction_response_id
            resolved_event_id
            unknown_reason
            owning_thread_id
            ledger_epoch
            source_file_id
            file_generation
            source_start_offset
            source_end_offset
            state_json
            reconciliation_state_json
            carry_phase
            carry_after_start_offset
            carry_after_turn_key
            carry_after_anomaly_id
            carry_after_fact_event_id
            carry_after_marker_start_offset
            carry_after_window_start_offset
            hold_reason
        )

        echo "Checking top-level token_usage_record and compacted parser routes..."
        parser_route="$(sed -n '/pub fn parse_line(/,/^[[:space:]]*pub const fn oversized_complete/p' src/codex/usage.rs)"
        if [[ "$parser_route" != *'Some("token_usage_record")'* || "$parser_route" != *'UsageRawRecord::ResponseUsage(ResponseUsageRecord {'* ]]; then
            echo "FAIL: top-level token_usage_record does not produce a response usage record" >&2
            failed=1
        else
            echo "PASS: token_usage_record produces a response usage record"
        fi
        if [[ "$parser_route" != *'Some("compacted")'* || "$parser_route" != *'UsageRawRecord::Compacted(CompactedRecord {'* ]]; then
            echo "FAIL: compacted parser record declaration or route is missing" >&2
            failed=1
        else
            echo "PASS: compacted records are parsed as CompactedRecord"
        fi
        normalized_record="$(sed -n '/^fn normalized_record(/,/^fn required_value(/p' src/codex/ingestion/usage_pipeline.rs)"
        if [[ "$normalized_record" != *'UsageRawRecord::ResponseUsage(record) => Some(UsageRecord::ResponseUsage {'* ]]; then
            echo "FAIL: token_usage_record response records do not enter the processor path" >&2
            failed=1
        else
            echo "PASS: token_usage_record response records enter the processor path"
        fi
        if [[ "$normalized_record" != *'UsageRawRecord::Compacted(record) => Some(UsageRecord::Compacted {'* ]]; then
            echo "FAIL: Compacted parser records are not converted into processor records" >&2
            failed=1
        else
            echo "PASS: Compacted parser records enter the processor path"
        fi

        echo "Checking runtime sources for v5 processor hooks..."
        if rg -n '\b(V5Runtime|LegacyV5Runtime|V5Processor|LegacyV5Processor|process_v5|process_legacy_v5|canonical_v5)\b' "${runtime_files[@]}"; then
            echo "FAIL: a separate v5 runtime processor hook is present" >&2
            failed=1
        else
            rg_status=$?
            if [[ "$rg_status" -ne 1 ]]; then
                echo "FAIL: could not scan the scoped runtime sources (rg exit $rg_status)" >&2
                failed=1
            else
                echo "PASS: no separate v5 runtime processor hook in the scoped runtime sources"
            fi
        fi

        echo "Checking canonical token aggregation for a second Compaction add..."
        token_totals="$(sed -n '/^pub struct TokenTotals {/,/^}/p' src/usage/aggregate.rs)"
        add_assign="$(sed -n '/^[[:space:]]*fn add_assign(&mut self, other: &Self)/,/^    }/p' src/usage/aggregate.rs)"
        if [[ -z "$token_totals" || -z "$add_assign" ]]; then
            echo "FAIL: could not isolate TokenTotals and its accumulation method" >&2
            failed=1
        elif [[ "$token_totals" == *compaction_tokens* || "$add_assign" == *compaction_tokens* ]]; then
            echo "FAIL: Compaction projection is included in canonical token accumulation" >&2
            failed=1
        else
            echo "PASS: Compaction remains a detail projection outside TokenTotals accumulation"
        fi

        echo "Checking canonical SQL schema and DTOs exclude private Compaction columns..."
        canonical_schema="$(sed -n '/^CREATE TABLE usage_events_v11 (/,/^);/p' src/storage/schema/0011_multi_source_core.sql)"
        token_usage_dto="$(sed -n '/^pub struct TokenUsageDto {/,/^}/p' src/api/query.rs)"
        token_totals="$(sed -n '/^pub struct TokenTotals {/,/^}/p' src/usage/aggregate.rs)"
        main_model_dto="$(sed -n '/^pub struct MainModelUsageDto {/,/^}/p' src/api/query.rs)"
        subagent_model_dto="$(sed -n '/^pub struct SubagentModelUsageDto {/,/^}/p' src/api/query.rs)"
        if [[ -z "$canonical_schema" || -z "$token_usage_dto" || -z "$token_totals" || -z "$main_model_dto" || -z "$subagent_model_dto" ]]; then
            echo "FAIL: could not isolate canonical schema or public DTO definitions" >&2
            failed=1
        else
            for column in "${private_columns[@]}"; do
                if printf '%s\n' "$canonical_schema" | rg -q "^[[:space:]]*$column[[:space:]]"; then
                    echo "FAIL: private column $column appears in canonical usage_events schema" >&2
                    failed=1
                fi
                for dto_name in TokenTotals TokenUsageDto MainModelUsageDto SubagentModelUsageDto; do
                    case "$dto_name" in
                        TokenTotals) dto="$token_totals" ;;
                        TokenUsageDto) dto="$token_usage_dto" ;;
                        MainModelUsageDto) dto="$main_model_dto" ;;
                        SubagentModelUsageDto) dto="$subagent_model_dto" ;;
                    esac
                    if printf '%s\n' "$dto" | rg -q "^[[:space:]]*pub[[:space:]]+$column[[:space:]]*:"; then
                        echo "FAIL: private column $column appears in public $dto_name" >&2
                        failed=1
                    fi
                done
            done
            if printf '%s\n' "$token_usage_dto" "$token_totals" | rg -q '^[[:space:]]*pub[[:space:]]+compaction_tokens[[:space:]]*:'; then
                echo "FAIL: compaction_tokens is embedded in canonical usage totals instead of a sibling model field" >&2
                failed=1
            elif [[ "$main_model_dto" != *'pub compaction_tokens: Option<i64>,'* || "$subagent_model_dto" != *'pub compaction_tokens: Option<i64>,'* ]]; then
                echo "FAIL: public Compaction projection is not a nullable sibling on both model DTOs" >&2
                failed=1
            else
                echo "PASS: canonical schema excludes private columns and DTOs expose only the nullable model-level projection"
            fi
        fi

        echo "Checking public detail layers for Codex-private SQL tables..."
        for table in "${private_tables[@]}"; do
            for file in "${public_files[@]}"; do
                # Public files keep their unit tests inline at the end; scan only runtime code.
                if matches="$(sed '/^#\[cfg(test)\]$/,$d' "$file" | rg -n -F "$table")"; then
                    echo "FAIL: private table $table is referenced by public runtime detail code in $file: $matches" >&2
                    failed=1
                else
                    rg_status=$?
                    if [[ "$rg_status" -ne 1 ]]; then
                        echo "FAIL: could not scan public runtime detail code for $table in $file (rg exit $rg_status)" >&2
                        failed=1
                    fi
                fi
            done
        done
        if [[ "$failed" -eq 0 ]]; then
            echo "PASS: public runtime detail code exposes the projection without new private table access"
        fi

        echo "Checking the integration suite defines its API acceptance smoke..."
        if rg -q '^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+compaction_system_acceptance_api_smoke[[:space:]]*\(' tests/codex_compaction_integration.rs; then
            echo "PASS: named API acceptance smoke is defined in the cargo test target"
        else
            echo "FAIL: compaction_system_acceptance_api_smoke is missing from tests/codex_compaction_integration.rs" >&2
            failed=1
        fi

        return "$failed"
}

if [[ "$#" -ne 0 ]]; then
    echo "Usage: bash scripts/ci/check_codex_compaction.sh" >&2
    exit 2
fi

STAMP="$(date -u '+%Y%m%dT%H%M%SZ')"
LOG_DIR="$ROOT_DIR/target/compaction-acceptance/phase5-${STAMP}-$$"
if ! mkdir -p "$LOG_DIR"; then
    echo "ERROR: cannot create persistent gate log directory: $LOG_DIR" >&2
    exit 2
fi

SUMMARY_FILE="$LOG_DIR/summary.tsv"
printf 'gate\tcommand_exit\tlogging_exit\tcommand\n' > "$SUMMARY_FILE"
OVERALL_EXIT=0

render_command() {
    local rendered
    printf -v rendered '%q ' "$@"
    printf '%s' "${rendered% }"
}

run_gate() {
    local name="$1"
    local workdir="$2"
    shift 2

    local args command log_file command_file exit_file logging_exit_file
    args="$(render_command "$@")"
    if [[ "$workdir" == "." ]]; then
        command="$args"
    else
        command="cd $(printf '%q' "$workdir") && $args"
    fi

    log_file="$LOG_DIR/$name.log"
    command_file="$LOG_DIR/$name.command.txt"
    exit_file="$LOG_DIR/$name.exit.txt"
    logging_exit_file="$LOG_DIR/$name.logging-exit.txt"

    if ! printf '%s\n' "$command" > "$command_file"; then
        echo "ERROR: cannot persist command for gate $name" >&2
        OVERALL_EXIT=1
        return
    fi
    {
        printf 'COMMAND: %s\n' "$command"
        printf 'STARTED_UTC: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    } > "$log_file" || {
        echo "ERROR: cannot persist output log for gate $name" >&2
        OVERALL_EXIT=1
        return
    }

    printf '\n=== %s ===\n$ %s\n' "$name" "$command"
    (
        cd "$ROOT_DIR/$workdir" || exit $?
        "$@"
    ) 2>&1 | tee -a "$log_file"
    local -a pipeline_status=("${PIPESTATUS[@]}")
    local command_exit="${pipeline_status[0]:-127}"
    local logging_exit="${pipeline_status[1]:-127}"

    printf '%s\n' "$command_exit" > "$exit_file" || OVERALL_EXIT=1
    printf '%s\n' "$logging_exit" > "$logging_exit_file" || OVERALL_EXIT=1
    {
        printf 'COMMAND_EXIT: %s\n' "$command_exit"
        printf 'LOGGING_EXIT: %s\n' "$logging_exit"
        printf 'FINISHED_UTC: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    } >> "$log_file" || OVERALL_EXIT=1
    printf '%s\t%s\t%s\t%s\n' "$name" "$command_exit" "$logging_exit" "$command" >> "$SUMMARY_FILE" || OVERALL_EXIT=1

    if [[ "$command_exit" -ne 0 || "$logging_exit" -ne 0 ]]; then
        OVERALL_EXIT=1
        printf 'FAIL: %s command exit=%s logging exit=%s\n' "$name" "$command_exit" "$logging_exit" >&2
    else
        printf 'PASS: %s\n' "$name"
    fi
}

printf 'Persistent Phase 5 logs: %s\n' "$LOG_DIR"
run_gate cargo_check . cargo check
run_gate cargo_fmt . cargo fmt --check
run_gate cargo_test . cargo test
run_gate frontend_npm_test frontend npm test
run_gate frontend_npm_build frontend npm run build
run_gate architecture_residue . architecture_residue_check

printf '\nGate summary: %s\n' "$SUMMARY_FILE"
if [[ "$OVERALL_EXIT" -ne 0 ]]; then
    echo "Phase 5 gate failed; see the per-gate logs. No gate was automatically rerun." >&2
    exit 1
fi

echo "Phase 5 gate passed."
