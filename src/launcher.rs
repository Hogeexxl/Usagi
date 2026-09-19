//! Bind-first launcher decisions shared by the binary and lifecycle tests.

use std::{fmt, io, net::SocketAddr, time::Duration};

use reqwest::{Client, header::HeaderValue};
use tokio::{net::TcpListener, time::Instant};

use crate::api::{APP_MARKER_HEADER, APP_MARKER_VALUE, listen_address};

const PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const PROBE_RETRY_DELAY: Duration = Duration::from_millis(40);
const WINDOWS_PORT_FALLBACK_COUNT: u16 = 32;

#[derive(Debug)]
pub enum BindOutcome {
    Listener(TcpListener),
    ExistingInstance(SocketAddr),
}

#[derive(Debug)]
pub enum LauncherError {
    Bind(io::Error),
    AddressInUse(SocketAddr),
    ProbeClient(reqwest::Error),
    NotReady(SocketAddr),
}

impl fmt::Display for LauncherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) => write!(formatter, "could not bind Usagi listener: {error}"),
            Self::AddressInUse(address) => write!(
                formatter,
                "{address} is already in use by another program (Usagi health marker not found)"
            ),
            Self::ProbeClient(error) => {
                write!(formatter, "could not create local health probe: {error}")
            }
            Self::NotReady(address) => {
                write!(
                    formatter,
                    "Usagi service at {address} did not become ready"
                )
            }
        }
    }
}

impl std::error::Error for LauncherError {}

/// Bind the fixed loopback listener before any Ledger or Scanner is created.
pub async fn bind_or_detect_existing() -> Result<BindOutcome, LauncherError> {
    bind_or_detect_existing_at(listen_address()).await
}

/// Windows uses the fixed production port as its first choice, but falls
/// forward through a small deterministic loopback range when another program
/// already owns that port. macOS continues to use `bind_or_detect_existing`
/// and therefore retains the fixed-port contract.
pub async fn bind_or_detect_existing_with_port_fallback() -> Result<BindOutcome, LauncherError> {
    bind_or_detect_existing_in_range(listen_address(), WINDOWS_PORT_FALLBACK_COUNT).await
}

async fn bind_or_detect_existing_in_range(
    preferred: SocketAddr,
    count: u16,
) -> Result<BindOutcome, LauncherError> {
    if count == 0 {
        return Err(LauncherError::AddressInUse(preferred));
    }
    let start_port = preferred.port();
    let end_port = start_port
        .checked_add(count - 1)
        .ok_or_else(|| LauncherError::AddressInUse(preferred))?;

    let mut first_available = None;
    for port in start_port..=end_port {
        let address = SocketAddr::new(preferred.ip(), port);
        match TcpListener::bind(address).await {
            Ok(listener) => {
                if first_available.is_none() {
                    first_available = Some(listener);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                if probe_health(address).await? {
                    return Ok(BindOutcome::ExistingInstance(address));
                }
            }
            Err(error) => return Err(LauncherError::Bind(error)),
        }
    }

    first_available
        .map(BindOutcome::Listener)
        .ok_or(LauncherError::AddressInUse(preferred))
}

async fn bind_or_detect_existing_at(address: SocketAddr) -> Result<BindOutcome, LauncherError> {
    match TcpListener::bind(address).await {
        Ok(listener) => Ok(BindOutcome::Listener(listener)),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            if probe_health(address).await? {
                Ok(BindOutcome::ExistingInstance(address))
            } else {
                Err(LauncherError::AddressInUse(address))
            }
        }
        Err(error) => Err(LauncherError::Bind(error)),
    }
}

/// Wait until the just-started Axum service answers its own health contract.
pub async fn wait_until_ready(address: SocketAddr) -> Result<(), LauncherError> {
    let client = local_client().map_err(LauncherError::ProbeClient)?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let url = format!("http://{address}/api/health");
    while Instant::now() < deadline {
        if let Ok(response) = client.get(&url).send().await
            && health_marker_matches(&response)
        {
            return Ok(());
        }
        tokio::time::sleep(PROBE_RETRY_DELAY).await;
    }
    Err(LauncherError::NotReady(address))
}

async fn probe_health(address: SocketAddr) -> Result<bool, LauncherError> {
    let client = local_client().map_err(LauncherError::ProbeClient)?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let url = format!("http://{address}/api/health");
    while Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(response) => return Ok(health_marker_matches(&response)),
            Err(_) => tokio::time::sleep(PROBE_RETRY_DELAY).await,
        }
    }
    Ok(false)
}

fn local_client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .no_proxy()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .build()
}

fn health_marker_matches(response: &reqwest::Response) -> bool {
    response.status().is_success()
        && response
            .headers()
            .get(APP_MARKER_HEADER)
            .is_some_and(|value| value == HeaderValue::from_static(APP_MARKER_VALUE))
}

#[cfg(test)]
mod tests {
    use axum::{Router, http::StatusCode, routing::get};
    use tokio::net::TcpListener;

    use super::*;

    async fn reserve_address() -> (SocketAddr, TcpListener) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        (address, listener)
    }

    #[tokio::test]
    async fn bind_first_returns_listener_without_starting_workers() {
        let outcome = bind_or_detect_existing_at("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        assert!(matches!(outcome, BindOutcome::Listener(_)));
    }

    #[tokio::test]
    async fn exact_health_marker_identifies_existing_usagi() {
        let (address, listener) = reserve_address().await;
        let app = Router::new().route(
            "/api/health",
            get(|| async {
                (
                    StatusCode::NO_CONTENT,
                    [(APP_MARKER_HEADER, APP_MARKER_VALUE)],
                )
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let outcome = bind_or_detect_existing_at(address).await.unwrap();
        assert!(matches!(outcome, BindOutcome::ExistingInstance(value) if value == address));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn occupied_non_usagi_port_is_an_explicit_error() {
        let (address, listener) = reserve_address().await;
        let error = bind_or_detect_existing_at(address).await.unwrap_err();
        assert!(matches!(error, LauncherError::AddressInUse(value) if value == address));
        drop(listener);
    }

    #[tokio::test]
    async fn fallback_binding_moves_forward_when_preferred_port_is_occupied() {
        let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let start = occupied.local_addr().unwrap();
        let next = start.port().checked_add(1).unwrap();
        let preferred = SocketAddr::new(start.ip(), start.port());

        let outcome = bind_or_detect_existing_in_range(preferred, 2)
            .await
            .unwrap();
        let BindOutcome::Listener(listener) = outcome else {
            panic!("expected fallback listener");
        };
        assert_eq!(listener.local_addr().unwrap().port(), next);
        drop(listener);
        drop(occupied);
    }

    #[test]
    fn production_listener_is_loopback_not_unspecified() {
        let address = listen_address();
        assert_eq!(address.ip().to_string(), "127.0.0.1");
        assert!(!address.ip().is_unspecified());
    }
}
