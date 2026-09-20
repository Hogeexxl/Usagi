//! Usage ingestion, epoch rebuild, carry, and read-only aggregate seams.

pub mod adapters;
pub mod aggregate;
pub mod analytics;
pub mod ledger;
pub mod normalized;
pub mod pipeline;
pub mod processor;
pub mod rebuild;
pub mod skills;

pub use normalized::{
    NormalizedTokenUsage, USAGE_CANONICAL_ALGORITHM_VERSION, USAGE_PARSER_VERSION,
    canonical_algorithm_for,
};
pub use skills::SkillUsageEvent;

pub use aggregate::{
    AggregateError, AggregateReader, FilterOptions, MAX_SESSION_ROWS, MainModelUsage,
    MainSessionDetail, ModelFilterOption, ModelUsageRow, ModelUsageRows, ProjectFilterOption,
    SessionCursor, SessionDetail, SessionPageRequest, SessionSnapshot, SessionSortField,
    SessionSortIndexItem, SessionSortOrder, SessionUsagePage, SessionUsageRow, SourceFilterOption,
    SubagentDetail, SubagentModelUsage, SummaryQuery, TimeRange, TokenTotals, UsageFilter,
    UsageSummary,
};
pub use pipeline::{
    CheckpointExpectation, ClassifiedOversizedUsageLine, ClassifiedUsageItem, ClassifiedUsageLine,
    FixedViewTail, PipelineDisposition, PipelineError, PlanAction, SourceContinuationState,
    SourceStateProof, TailStatus, UsagePipeline, UsagePipelinePlan, UsageSourceCommitDto,
};
pub use processor::{
    Anomaly, AnomalyCode, ClosedTurn, EventKind, GapKind, Occurrence, Ownership, ProcessResult,
    ProcessorError, TurnEndStatus, TurnModelState, TurnState, UsageContext, UsageEvent,
    UsageProcessor, UsageRecord, UsageSourceState, UsageValue,
};
pub use rebuild::{
    ActivationOutcome, BuildSnapshot, CompletionStatus, ManifestEntry, ProgressOutcome,
    RebuildError, RebuildLedger, SourceProgress, TailProof,
};

pub use ledger::{
    CarryStepOutcome, SessionDetailSnapshot, SessionRowsSnapshot, UsageBuildScanProof,
    UsageCommitOutcome, UsageLedger, UsageLedgerError, UsageScanState, UsageSourceScanPlan,
};
