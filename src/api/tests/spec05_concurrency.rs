use std::{sync::mpsc, time::Duration};

use axum::{
    body::to_bytes,
    http::{Method, StatusCode},
};
use rusqlite::Connection;
use serde_json::Value;

use super::support::ApiFixture;

#[tokio::test(flavor = "current_thread")]
async fn t_s05_021_blocked_sqlite_query_does_not_block_tokio_executor() {
    let fixture = ApiFixture::new("executor-isolation");
    let ledger = fixture.ledger.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let holder = std::thread::spawn(move || {
        let _guard = ledger.connection().unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();

    let app = fixture.app.clone();
    let blocked = tokio::spawn(async move {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        app.oneshot(
            Request::builder()
                .uri("/api/revision")
                .header("host", "127.0.0.1:3210")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });

    tokio::task::yield_now().await;
    assert!(
        !blocked.is_finished(),
        "the SQLite query should still be waiting for the Ledger mutex"
    );

    // If the synchronous SQLite work ran directly on this single-thread Tokio
    // executor, this health request and timer could not complete while the
    // mutex-holder sleeps.
    let health = tokio::time::timeout(
        Duration::from_millis(80),
        fixture.call(Method::GET, "/api/health", &[]),
    )
    .await
    .expect("Tokio executor was starved by synchronous SQLite work");
    assert_eq!(health.status(), StatusCode::NO_CONTENT);

    release_tx.send(()).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), blocked)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    holder.join().unwrap();
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s05_019_real_sqlite_busy_refresh_returns_only_safe_code() {
    let fixture = ApiFixture::new("busy-safe-error");

    // Shorten only this test connection's busy wait. No production behavior is
    // changed; the actual competing writer is a second SQLite connection.
    fixture
        .ledger
        .connection()
        .unwrap()
        .busy_timeout(Duration::from_millis(20))
        .unwrap();
    let blocker = Connection::open(fixture.ledger.database_path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();

    let response = fixture
        .call(Method::POST, "/api/refresh", &[("x-usagi-request", "1")])
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "DATABASE_BUSY");
    let rendered = String::from_utf8(bytes.to_vec()).unwrap();
    for forbidden in ["sqlite", "BEGIN IMMEDIATE", "mu.sqlite3", "codex", "prompt"] {
        assert!(
            !rendered
                .to_ascii_lowercase()
                .contains(&forbidden.to_ascii_lowercase())
        );
    }

    blocker.execute_batch("ROLLBACK").unwrap();
    fixture.scanner.shutdown().unwrap();
}
