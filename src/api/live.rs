//! Manual-refresh and revision-notification logic shared by the HTTP layer.
//!
//! HTTP extraction and SSE response construction stay in the router module.
//! This module owns the durable scanner acknowledgement mapping and the
//! bounded, latest-value revision stream.

use std::sync::Arc;

use serde::Serialize;

use crate::ingestion::{
    CommitFailureKind, RequestDisposition, ScanHandle, ScanRequestError, ScanTrigger,
};

pub const REFRESH_HEADER_VALUE: &str = "1";
pub const SSE_ACCEL_BUFFERING: &str = "no";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RefreshAccepted {
    pub http_status: u16,
    pub disposition: &'static str,
    pub scan_id: String,
    pub status_revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveError {
    Forbidden,
    ScannerUnavailable,
    DatabaseBusy,
    ScanStartFailed,
    ScanEnqueueFailed,
}

trait ManualScanRequester: Send + Sync + 'static {
    fn request_manual(&self) -> Result<RequestDisposition, ScanRequestError>;
}

impl ManualScanRequester for ScanHandle {
    fn request_manual(&self) -> Result<RequestDisposition, ScanRequestError> {
        self.request(ScanTrigger::Manual)
    }
}

/// Waits only for the coordinator's persisted Started/Coalesced acknowledgement.
/// The blocking coordinator channel is kept off the Tokio executor.
pub async fn refresh(
    header_value: Option<&str>,
    scanner: ScanHandle,
) -> Result<RefreshAccepted, LiveError> {
    if header_value != Some(REFRESH_HEADER_VALUE) {
        return Err(LiveError::Forbidden);
    }
    refresh_with(header_value, Arc::new(scanner)).await
}

async fn refresh_with(
    header_value: Option<&str>,
    scanner: Arc<dyn ManualScanRequester>,
) -> Result<RefreshAccepted, LiveError> {
    if header_value != Some(REFRESH_HEADER_VALUE) {
        return Err(LiveError::Forbidden);
    }
    let disposition = tokio::task::spawn_blocking(move || scanner.request_manual())
        .await
        .map_err(|_| LiveError::ScanStartFailed)??;
    Ok(match disposition {
        RequestDisposition::Started {
            scan_id,
            started_status_revision,
        } => RefreshAccepted {
            http_status: 202,
            disposition: "started",
            scan_id,
            status_revision: started_status_revision,
        },
        RequestDisposition::Coalesced {
            followup_scan_id,
            enqueued_status_revision,
        } => RefreshAccepted {
            http_status: 200,
            disposition: "coalesced",
            scan_id: followup_scan_id,
            status_revision: enqueued_status_revision,
        },
    })
}

impl From<ScanRequestError> for LiveError {
    fn from(error: ScanRequestError) -> Self {
        match error {
            ScanRequestError::Recovering | ScanRequestError::ShuttingDown => {
                Self::ScannerUnavailable
            }
            ScanRequestError::StartCommitFailed {
                kind: CommitFailureKind::Busy,
            }
            | ScanRequestError::EnqueueCommitFailed {
                kind: CommitFailureKind::Busy,
            } => Self::DatabaseBusy,
            ScanRequestError::StartCommitFailed {
                kind: CommitFailureKind::Internal,
            } => Self::ScanStartFailed,
            ScanRequestError::EnqueueCommitFailed {
                kind: CommitFailureKind::Internal,
            } => Self::ScanEnqueueFailed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex};

    use super::*;

    struct RequestStub {
        result: Mutex<Option<Result<RequestDisposition, ScanRequestError>>>,
        calls: Mutex<usize>,
        release: (Mutex<bool>, Condvar),
    }

    impl RequestStub {
        fn ready(result: Result<RequestDisposition, ScanRequestError>) -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Some(result)),
                calls: Mutex::new(0),
                release: (Mutex::new(true), Condvar::new()),
            })
        }

        fn blocked(result: Result<RequestDisposition, ScanRequestError>) -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Some(result)),
                calls: Mutex::new(0),
                release: (Mutex::new(false), Condvar::new()),
            })
        }

        fn release(&self) {
            *self.release.0.lock().unwrap() = true;
            self.release.1.notify_all();
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl ManualScanRequester for RequestStub {
        fn request_manual(&self) -> Result<RequestDisposition, ScanRequestError> {
            *self.calls.lock().unwrap() += 1;
            let mut released = self.release.0.lock().unwrap();
            while !*released {
                released = self.release.1.wait(released).unwrap();
            }
            self.result.lock().unwrap().take().unwrap()
        }
    }

    #[tokio::test]
    async fn refresh_validates_before_request_and_maps_durable_ack_without_waiting_for_scan() {
        let never_called = RequestStub::ready(Ok(RequestDisposition::Started {
            scan_id: "unused".to_owned(),
            started_status_revision: 1,
        }));
        for (header, expected) in [
            (None, LiveError::Forbidden),
            (Some("0"), LiveError::Forbidden),
        ] {
            assert_eq!(
                refresh_with(header, never_called.clone()).await,
                Err(expected)
            );
        }
        assert_eq!(never_called.calls(), 0);

        let started = RequestStub::blocked(Ok(RequestDisposition::Started {
            scan_id: "scan-started".to_owned(),
            started_status_revision: 8,
        }));
        let task = tokio::spawn(refresh_with(Some("1"), started.clone()));
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        started.release();
        assert_eq!(
            task.await.unwrap().unwrap(),
            RefreshAccepted {
                http_status: 202,
                disposition: "started",
                scan_id: "scan-started".to_owned(),
                status_revision: 8,
            }
        );

        let coalesced = RequestStub::ready(Ok(RequestDisposition::Coalesced {
            followup_scan_id: "followup".to_owned(),
            enqueued_status_revision: 9,
        }));
        assert_eq!(
            refresh_with(Some("1"), coalesced).await.unwrap(),
            RefreshAccepted {
                http_status: 200,
                disposition: "coalesced",
                scan_id: "followup".to_owned(),
                status_revision: 9,
            }
        );
    }

    #[tokio::test]
    async fn refresh_maps_all_safe_coordinator_failures() {
        for (error, expected) in [
            (ScanRequestError::Recovering, LiveError::ScannerUnavailable),
            (
                ScanRequestError::ShuttingDown,
                LiveError::ScannerUnavailable,
            ),
            (
                ScanRequestError::StartCommitFailed {
                    kind: CommitFailureKind::Busy,
                },
                LiveError::DatabaseBusy,
            ),
            (
                ScanRequestError::EnqueueCommitFailed {
                    kind: CommitFailureKind::Busy,
                },
                LiveError::DatabaseBusy,
            ),
            (
                ScanRequestError::StartCommitFailed {
                    kind: CommitFailureKind::Internal,
                },
                LiveError::ScanStartFailed,
            ),
            (
                ScanRequestError::EnqueueCommitFailed {
                    kind: CommitFailureKind::Internal,
                },
                LiveError::ScanEnqueueFailed,
            ),
        ] {
            let stub = RequestStub::ready(Err(error));
            assert_eq!(refresh_with(Some("1"), stub).await, Err(expected));
        }
    }
}
