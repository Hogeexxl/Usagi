//! Read-only facade over the canonical usage ledger.

use crate::storage::{Ledger, StorageError};

use super::aggregate::{
    AggregateError, AggregateReader, FilterOptions, ModelUsageRows, SessionDetail,
    SessionPageRequest, SessionSortField, SessionSortOrder, SessionUsagePage, SummaryQuery,
    TimeRange, UsageFilter, UsageSummary,
};

#[derive(Debug)]
pub enum UsageLedgerError {
    Storage(StorageError),
    Aggregate(AggregateError),
    Invalid(&'static str),
    StaleDataRevision,
}

impl From<StorageError> for UsageLedgerError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<AggregateError> for UsageLedgerError {
    fn from(value: AggregateError) -> Self {
        Self::Aggregate(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageSnapshot<T> {
    pub data_revision: i64,
    pub value: T,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionSnapshot {
    pub data_revision: i64,
    pub sort_index: Vec<super::aggregate::SessionSortIndexItem>,
    pub rows: Vec<super::aggregate::SessionUsageRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRowsSnapshot {
    pub data_revision: i64,
    pub rows: Vec<super::aggregate::SessionUsageRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionDetailSnapshot {
    pub data_revision: i64,
    pub value: SessionDetail,
}

pub struct UsageLedger<'a> {
    ledger: &'a Ledger,
    session_error_sidecars: &'a [&'a dyn super::aggregate::SessionErrorSidecar],
}

impl<'a> UsageLedger<'a> {
    pub fn new(
        ledger: &'a Ledger,
        session_error_sidecars: &'a [&'a dyn super::aggregate::SessionErrorSidecar],
    ) -> Self {
        Self {
            ledger,
            session_error_sidecars,
        }
    }

    pub fn summary(&self, query: SummaryQuery) -> Result<UsageSummary, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            Ok(AggregateReader::new(transaction, self.session_error_sidecars).summary(query)?)
        })
    }

    pub fn sessions(
        &self,
        range: TimeRange,
        request: SessionPageRequest,
    ) -> Result<SessionUsagePage, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            Ok(
                AggregateReader::new(transaction, self.session_error_sidecars)
                    .sessions(range, request)?,
            )
        })
    }

    pub fn models(&self, range: TimeRange) -> Result<ModelUsageRows, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            Ok(AggregateReader::new(transaction, self.session_error_sidecars).models(range)?)
        })
    }

    pub fn summary_snapshot(
        &self,
        query: SummaryQuery,
    ) -> Result<UsageSnapshot<UsageSummary>, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            let data_revision = snapshot_meta(transaction)?;
            let value =
                AggregateReader::new(transaction, self.session_error_sidecars).summary(query)?;
            Ok(UsageSnapshot {
                data_revision,
                value,
            })
        })
    }

    pub fn sessions_snapshot(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        seed_sort_field: SessionSortField,
        seed_sort_order: SessionSortOrder,
    ) -> Result<SessionSnapshot, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            let data_revision = snapshot_meta(transaction)?;
            let value = AggregateReader::new(transaction, self.session_error_sidecars)
                .session_snapshot(range, &filter, seed_sort_field, seed_sort_order)?;
            Ok(SessionSnapshot {
                data_revision,
                sort_index: value.sort_index,
                rows: value.rows,
            })
        })
    }

    pub fn session_rows_snapshot(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        expected_data_revision: Option<i64>,
        root_session_ids: Vec<String>,
    ) -> Result<SessionRowsSnapshot, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            let data_revision = snapshot_meta(transaction)?;
            if expected_data_revision.is_some_and(|expected| expected != data_revision) {
                return Err(UsageLedgerError::StaleDataRevision);
            }
            let rows = AggregateReader::new(transaction, self.session_error_sidecars)
                .session_rows(range, &filter, &root_session_ids)?;
            Ok(SessionRowsSnapshot {
                data_revision,
                rows,
            })
        })
    }

    pub fn session_detail_snapshot(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        expected_data_revision: Option<i64>,
        root_session_id: String,
    ) -> Result<SessionDetailSnapshot, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            let data_revision = snapshot_meta(transaction)?;
            if expected_data_revision.is_some_and(|expected| expected != data_revision) {
                return Err(UsageLedgerError::StaleDataRevision);
            }
            let value = AggregateReader::new(transaction, self.session_error_sidecars)
                .session_detail(range, &filter, &root_session_id)?;
            Ok(SessionDetailSnapshot {
                data_revision,
                value,
            })
        })
    }

    pub fn models_snapshot(
        &self,
        range: TimeRange,
    ) -> Result<UsageSnapshot<ModelUsageRows>, UsageLedgerError> {
        self.models_snapshot_filtered(range, &UsageFilter::default())
    }

    pub fn models_snapshot_filtered(
        &self,
        range: TimeRange,
        filter: &UsageFilter,
    ) -> Result<UsageSnapshot<ModelUsageRows>, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            let data_revision = snapshot_meta(transaction)?;
            let value = AggregateReader::new(transaction, self.session_error_sidecars)
                .models_filtered(range, filter)?;
            Ok(UsageSnapshot {
                data_revision,
                value,
            })
        })
    }

    pub fn filter_options_snapshot(
        &self,
    ) -> Result<UsageSnapshot<FilterOptions>, UsageLedgerError> {
        self.ledger.with_read_transaction(|transaction| {
            let data_revision = snapshot_meta(transaction)?;
            let value =
                AggregateReader::new(transaction, self.session_error_sidecars).filter_options()?;
            Ok(UsageSnapshot {
                data_revision,
                value,
            })
        })
    }
}

fn snapshot_meta(connection: &rusqlite::Connection) -> Result<i64, UsageLedgerError> {
    let value = connection
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(StorageError::sqlite)?;
    if value < 0 {
        return Err(UsageLedgerError::Invalid("invalid usage snapshot metadata"));
    }
    Ok(value)
}
