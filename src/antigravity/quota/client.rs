use std::{fmt, path::Path, process::Command, time::Duration};

use reqwest::{Client, StatusCode, redirect::Policy};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::{auth, mapper};
use crate::codex::quota::CodexQuotaWindow;

const CLOUD_CODE_BASES: [&str; 2] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const QUOTA_SUMMARY_PATH: &str = "/v1internal:retrieveUserQuotaSummary";
const LOAD_CODE_ASSIST_PATH: &str = "/v1internal:loadCodeAssist";
const LOAD_CODE_ASSIST_USER_AGENT: &str = "antigravity";
const GOOGLE_OAUTH_URL: &str = "https://oauth2.googleapis.com/token";
// These installed-app OAuth client credentials are the public pair used by Antigravity itself.
const GOOGLE_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const GOOGLE_CLIENT_SECRET: &str = "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";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuotaPayload {
    pub(crate) account_email: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(crate) session: Option<CodexQuotaWindow>,
    pub(crate) weekly: Option<CodexQuotaWindow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuotaFetchError {
    AuthRequired,
    Unavailable,
}

impl fmt::Display for QuotaFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AuthRequired => "Antigravity authentication is required",
            Self::Unavailable => "Antigravity quota is unavailable",
        })
    }
}

struct CachedToken {
    source_refresh_token: String,
    access_token: String,
    expires_at_ms: i64,
}

pub struct AntigravityQuotaClient {
    local_client: Client,
    remote_client: Client,
    cached_token: Mutex<Option<CachedToken>>,
}

impl AntigravityQuotaClient {
    pub fn new() -> Result<Self, reqwest::Error> {
        let local_client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .danger_accept_invalid_certs(true)
            .build()?;
        let remote_client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .build()?;
        Ok(Self {
            local_client,
            remote_client,
            cached_token: Mutex::new(None),
        })
    }

    pub(crate) async fn fetch(&self, now_ms: i64) -> Result<QuotaPayload, QuotaFetchError> {
        if let Some(payload) = self.fetch_language_server().await {
            let cloud_plan = if payload.plan_type.is_none() {
                self.fetch_cloud_plan(now_ms).await
            } else {
                None
            };
            return Ok(fill_missing_cloud_plan(payload, cloud_plan));
        }
        self.fetch_cloud_code(now_ms).await
    }

    async fn fetch_language_server(&self) -> Option<QuotaPayload> {
        let servers = tokio::task::spawn_blocking(discover_language_servers)
            .await
            .ok()??;
        for server in servers {
            for (scheme, port) in server.endpoints() {
                let Some(summary) = self
                    .call_language_server(&server, &scheme, port, "RetrieveUserQuotaSummary")
                    .await
                else {
                    continue;
                };
                let Some(mapped) = mapper::parse_summary(&summary) else {
                    continue;
                };
                let status = self
                    .call_language_server(&server, &scheme, port, "GetUserStatus")
                    .await;
                let plan_type = status.as_ref().and_then(mapper::parse_ls_plan);
                return Some(QuotaPayload {
                    account_email: None,
                    plan_type,
                    session: mapped.session,
                    weekly: mapped.weekly,
                });
            }
        }
        None
    }

    async fn call_language_server(
        &self,
        server: &LanguageServer,
        scheme: &str,
        port: u16,
        method: &str,
    ) -> Option<Value> {
        let url = format!(
            "{scheme}://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/{method}"
        );
        let mut request = self
            .local_client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("Connect-Protocol-Version", "1")
            .json(&json!({
                "metadata": {
                    "ideName": "antigravity",
                    "extensionName": "antigravity",
                    "ideVersion": "unknown",
                    "locale": "en"
                }
            }));
        if !server.csrf.is_empty() {
            request = request.header("x-codeium-csrf-token", &server.csrf);
        }
        let response = request.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().await.ok()
    }

    async fn fetch_cloud_code(&self, now_ms: i64) -> Result<QuotaPayload, QuotaFetchError> {
        let raw = tokio::task::spawn_blocking(auth::read_keychain_token)
            .await
            .map_err(|_| QuotaFetchError::Unavailable)?
            .map_err(map_auth_read_error)?;
        let credentials = raw.ok_or(QuotaFetchError::AuthRequired)?;
        let (mut access_token, was_refreshed) = self.access_token(&credentials, now_ms).await?;

        let summary = match self
            .call_cloud_code(QUOTA_SUMMARY_PATH, &access_token, "antigravity")
            .await
        {
            Ok(summary) => summary,
            Err(CloudCodeError::Unauthorized) if !was_refreshed => {
                let refresh_token = credentials
                    .refresh_token
                    .as_deref()
                    .ok_or(QuotaFetchError::AuthRequired)?;
                access_token = self.refresh_token(refresh_token, now_ms).await?;
                self.call_cloud_code(QUOTA_SUMMARY_PATH, &access_token, "antigravity")
                    .await
                    .map_err(map_cloud_error)?
            }
            Err(error) => return Err(map_cloud_error(error)),
        };
        let mapped = mapper::parse_summary(&summary).ok_or(QuotaFetchError::Unavailable)?;
        let load_code_assist = self
            .call_cloud_code(
                LOAD_CODE_ASSIST_PATH,
                &access_token,
                LOAD_CODE_ASSIST_USER_AGENT,
            )
            .await
            .ok();
        let plan_type = load_code_assist.as_ref().and_then(mapper::parse_cloud_plan);
        Ok(QuotaPayload {
            account_email: credentials.account_email,
            plan_type,
            session: mapped.session,
            weekly: mapped.weekly,
        })
    }

    async fn fetch_cloud_plan(&self, now_ms: i64) -> Option<String> {
        let raw = tokio::task::spawn_blocking(auth::read_keychain_token)
            .await
            .ok()?
            .ok()?;
        let credentials = raw?;
        let (mut access_token, was_refreshed) =
            self.access_token(&credentials, now_ms).await.ok()?;
        let response = match self
            .call_cloud_code(
                LOAD_CODE_ASSIST_PATH,
                &access_token,
                LOAD_CODE_ASSIST_USER_AGENT,
            )
            .await
        {
            Ok(response) => response,
            Err(CloudCodeError::Unauthorized) if !was_refreshed => {
                let refresh_token = credentials.refresh_token.as_deref()?;
                access_token = self.refresh_token(refresh_token, now_ms).await.ok()?;
                self.call_cloud_code(
                    LOAD_CODE_ASSIST_PATH,
                    &access_token,
                    LOAD_CODE_ASSIST_USER_AGENT,
                )
                .await
                .ok()?
            }
            Err(CloudCodeError::Unauthorized | CloudCodeError::Unavailable) => return None,
        };
        mapper::parse_cloud_plan(&response)
    }

    async fn access_token(
        &self,
        credentials: &auth::KeychainToken,
        now_ms: i64,
    ) -> Result<(String, bool), QuotaFetchError> {
        if let Some(access_token) = credentials
            .access_token
            .as_ref()
            .filter(|_| is_usable(credentials.expires_at_ms, now_ms))
        {
            return Ok((access_token.clone(), false));
        }

        if let Some(refresh_token) = credentials.refresh_token.as_deref() {
            let cached = self.cached_token.lock().await;
            if let Some(cached) = cached.as_ref().filter(|cached| {
                cached.source_refresh_token == refresh_token
                    && cached.expires_at_ms > now_ms.saturating_add(TOKEN_REFRESH_BUFFER_MS)
            }) {
                return Ok((cached.access_token.clone(), false));
            }
            drop(cached);
            let token = self.refresh_token(refresh_token, now_ms).await?;
            return Ok((token, true));
        }

        credentials
            .access_token
            .clone()
            .map(|token| (token, false))
            .ok_or(QuotaFetchError::AuthRequired)
    }

    async fn refresh_token(
        &self,
        refresh_token: &str,
        now_ms: i64,
    ) -> Result<String, QuotaFetchError> {
        let response = self
            .remote_client
            .post(GOOGLE_OAUTH_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", GOOGLE_CLIENT_ID),
                ("client_secret", GOOGLE_CLIENT_SECRET),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|_| QuotaFetchError::Unavailable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(if is_auth_failure(status) {
                QuotaFetchError::AuthRequired
            } else {
                QuotaFetchError::Unavailable
            });
        }
        let response: Value = response
            .json()
            .await
            .map_err(|_| QuotaFetchError::Unavailable)?;
        let access_token = response
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or(QuotaFetchError::Unavailable)?;
        let expires_in = response
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(3_600)
            .max(0);
        let expires_at_ms = now_ms.saturating_add(expires_in.saturating_mul(1_000));
        let mut cached = self.cached_token.lock().await;
        *cached = Some(CachedToken {
            source_refresh_token: refresh_token.to_owned(),
            access_token: access_token.to_owned(),
            expires_at_ms,
        });
        Ok(access_token.to_owned())
    }

    async fn call_cloud_code(
        &self,
        path: &str,
        access_token: &str,
        user_agent: &str,
    ) -> Result<Value, CloudCodeError> {
        for base in CLOUD_CODE_BASES {
            let response = self
                .remote_client
                .post(format!("{base}{path}"))
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::USER_AGENT, user_agent)
                .bearer_auth(access_token)
                .json(&json!({}))
                .send()
                .await;
            let Ok(response) = response else {
                continue;
            };
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(CloudCodeError::Unauthorized);
            }
            if status.is_success() {
                return response
                    .json()
                    .await
                    .map_err(|_| CloudCodeError::Unavailable);
            }
        }
        Err(CloudCodeError::Unavailable)
    }
}

impl super::QuotaProvider for AntigravityQuotaClient {
    fn fetch<'a>(
        &'a self,
        now_ms: i64,
    ) -> futures_util::future::BoxFuture<'a, Result<QuotaPayload, QuotaFetchError>> {
        Box::pin(async move { AntigravityQuotaClient::fetch(self, now_ms).await })
    }
}

#[derive(Clone, Copy)]
enum CloudCodeError {
    Unauthorized,
    Unavailable,
}

fn map_cloud_error(error: CloudCodeError) -> QuotaFetchError {
    match error {
        CloudCodeError::Unauthorized => QuotaFetchError::AuthRequired,
        CloudCodeError::Unavailable => QuotaFetchError::Unavailable,
    }
}

fn map_auth_read_error(error: auth::AuthReadError) -> QuotaFetchError {
    match error {
        auth::AuthReadError::Missing | auth::AuthReadError::Invalid => {
            QuotaFetchError::AuthRequired
        }
        auth::AuthReadError::Unavailable => QuotaFetchError::Unavailable,
    }
}

fn is_auth_failure(status: StatusCode) -> bool {
    status.is_client_error()
        && status != StatusCode::REQUEST_TIMEOUT
        && status != StatusCode::TOO_MANY_REQUESTS
}

fn is_usable(expires_at_ms: Option<i64>, now_ms: i64) -> bool {
    expires_at_ms.is_none_or(|expiry| expiry > now_ms.saturating_add(TOKEN_REFRESH_BUFFER_MS))
}

fn fill_missing_cloud_plan(mut payload: QuotaPayload, cloud_plan: Option<String>) -> QuotaPayload {
    if payload.plan_type.is_none() {
        payload.plan_type = cloud_plan;
    }
    payload
}

struct LanguageServer {
    csrf: String,
    ports: Vec<u16>,
    extension_port: Option<u16>,
}

impl LanguageServer {
    fn endpoints(&self) -> Vec<(String, u16)> {
        let mut endpoints = Vec::new();
        for port in &self.ports {
            endpoints.push(("https".to_owned(), *port));
            endpoints.push(("http".to_owned(), *port));
        }
        if let Some(port) = self.extension_port {
            endpoints.push(("http".to_owned(), port));
        }
        endpoints
    }
}

fn discover_language_servers() -> Option<Vec<LanguageServer>> {
    let output = Command::new("/bin/ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let process_list = String::from_utf8(output.stdout).ok()?;
    Some(discover_language_servers_from_process_list(
        &process_list,
        listening_ports,
    ))
}

fn discover_language_servers_from_process_list<F>(
    process_list: &str,
    ports_for_pid: F,
) -> Vec<LanguageServer>
where
    F: Fn(u32) -> Vec<u16> + Copy,
{
    let mut servers = discover_processes(
        process_list,
        "language_server",
        &["antigravity", "antigravity-ide"],
        Some("--csrf_token"),
        Some("--extension_server_port"),
        ports_for_pid,
    );
    servers.extend(discover_processes(
        process_list,
        "agy",
        &[],
        None,
        None,
        ports_for_pid,
    ));
    servers
}

fn discover_processes<F>(
    process_list: &str,
    process_name: &str,
    markers: &[&str],
    csrf_flag: Option<&str>,
    extension_flag: Option<&str>,
    ports_for_pid: F,
) -> Vec<LanguageServer>
where
    F: Fn(u32) -> Vec<u16>,
{
    let mut servers = Vec::new();
    for line in process_list.lines() {
        let line = line.trim();
        let Some((pid_text, command)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_text.trim().parse::<u32>() else {
            continue;
        };
        if !command_matches_process(command.trim(), process_name)
            || !matches_markers(command.trim(), markers)
        {
            continue;
        }
        let csrf = csrf_flag
            .and_then(|flag| extract_flag(command.trim(), flag))
            .unwrap_or_default();
        if csrf_flag.is_some() && csrf.is_empty() {
            continue;
        }
        let extension_port = extension_flag
            .and_then(|flag| extract_flag(command.trim(), flag))
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port != 0);
        let ports = ports_for_pid(pid);
        if ports.is_empty() && extension_port.is_none() {
            continue;
        }
        servers.push(LanguageServer {
            csrf,
            ports,
            extension_port,
        });
    }
    servers
}

fn command_matches_process(command: &str, process_name: &str) -> bool {
    let argv0 = command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(['\'', '"']);
    let executable = Path::new(argv0)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if executable == process_name {
        return true;
    }
    if process_name.len() >= 8 && executable.starts_with(&format!("{process_name}_")) {
        return true;
    }
    let command = command.to_ascii_lowercase();
    command.ends_with(&format!("/{process_name}"))
        || command.contains(&format!("/{process_name} "))
        || command.contains(&format!("/{process_name}\t"))
}

fn matches_markers(command: &str, markers: &[&str]) -> bool {
    if markers.is_empty() {
        return true;
    }
    let values = ["--ide_name", "--override_ide_name", "--app_data_dir"]
        .into_iter()
        .filter_map(|flag| extract_flag(command, flag))
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if !values.is_empty() {
        return markers
            .iter()
            .any(|marker| values.iter().any(|value| value == marker));
    }
    let command = command.to_ascii_lowercase();
    markers.iter().any(|marker| {
        command.contains(&format!("/{marker}/")) || command.ends_with(&format!("/{marker}"))
    })
}

fn extract_flag(command: &str, flag: &str) -> Option<String> {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    let flag_eq = format!("{flag}=");
    for (index, part) in parts.iter().enumerate() {
        if *part == flag {
            return parts.get(index + 1).map(|value| (*value).to_owned());
        }
        if let Some(value) = part.strip_prefix(&flag_eq) {
            return Some(value.to_owned());
        }
    }
    None
}

fn listening_ports(pid: u32) -> Vec<u16> {
    let output = ["/usr/sbin/lsof", "/usr/bin/lsof"].iter().find_map(|path| {
        Command::new(path)
            .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pid.to_string()])
            .output()
            .ok()
            .filter(|output| output.status.success())
    });
    let Some(output) = output else {
        return Vec::new();
    };
    let Ok(output) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    let mut ports = output
        .lines()
        .filter(|line| line.contains("LISTEN"))
        .filter_map(|line| {
            line.split_whitespace().rev().find_map(|token| {
                let (_, port) = token.rsplit_once(':')?;
                port.parse::<u16>().ok().filter(|port| *port != 0)
            })
        })
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_code_assist_uses_antigravity_user_agent() {
        assert_eq!(LOAD_CODE_ASSIST_USER_AGENT, "antigravity");
    }

    #[test]
    fn cloud_plan_fills_only_a_missing_language_server_plan() {
        let local = QuotaPayload {
            account_email: Some("antigravity.fixture@example.test".to_owned()),
            plan_type: None,
            session: Some(CodexQuotaWindow {
                used_percent: 20.0,
                remaining_percent: 80.0,
                limit_window_seconds: 18_000,
                reset_at_ms: Some(1_790_150_400_000),
            }),
            weekly: Some(CodexQuotaWindow {
                used_percent: 30.0,
                remaining_percent: 70.0,
                limit_window_seconds: 604_800,
                reset_at_ms: None,
            }),
        };

        let filled = fill_missing_cloud_plan(local.clone(), Some("Pro".to_owned()));
        assert_eq!(filled.plan_type.as_deref(), Some("Pro"));
        assert_eq!(filled.account_email, local.account_email);
        assert_eq!(filled.session, local.session);
        assert_eq!(filled.weekly, local.weekly);

        let authoritative = fill_missing_cloud_plan(
            QuotaPayload {
                plan_type: Some("Ultra".to_owned()),
                ..local
            },
            Some("Pro".to_owned()),
        );
        assert_eq!(authoritative.plan_type.as_deref(), Some("Ultra"));
    }

    #[test]
    fn discovery_keeps_agy_servers_after_antigravity_servers() {
        let process_list = "101 /Applications/Antigravity.app/language_server --csrf_token app-csrf --override_ide_name antigravity --extension_server_port=52168\n102 /Users/test/bin/agy\n";
        let servers = discover_language_servers_from_process_list(process_list, |_| vec![52168]);

        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].csrf, "app-csrf");
        assert_eq!(servers[1].csrf, "");
    }

    #[test]
    fn command_flags_support_both_value_forms_without_logging_them() {
        assert!(command_matches_process(
            "/Applications/Antigravity.app/language_server --csrf_token hidden",
            "language_server"
        ));
        assert!(!command_matches_process(
            "/Applications/Other.app/language_server --csrf_token hidden",
            "agy"
        ));
        assert_eq!(
            extract_flag("language_server --csrf_token hidden", "--csrf_token").as_deref(),
            Some("hidden")
        );
        assert_eq!(
            extract_flag(
                "agy --extension_server_port=50123",
                "--extension_server_port"
            )
            .as_deref(),
            Some("50123")
        );
        assert!(matches_markers(
            "language_server --override_ide_name antigravity",
            &["antigravity"]
        ));
        assert!(!matches_markers(
            "language_server --override_ide_name antigravity-next",
            &["antigravity"]
        ));
    }
}
