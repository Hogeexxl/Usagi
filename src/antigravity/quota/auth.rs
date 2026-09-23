use std::process::Command;

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::DateTime;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthReadError {
    Missing,
    Unavailable,
    Invalid,
}

pub(crate) struct KeychainToken {
    pub(crate) access_token: Option<String>,
    pub(crate) refresh_token: Option<String>,
    pub(crate) expires_at_ms: Option<i64>,
    pub(crate) account_email: Option<String>,
}

pub(crate) fn read_keychain_token() -> Result<Option<KeychainToken>, AuthReadError> {
    let raw = read_keychain_secret()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    parse_keychain_token(&raw)
        .map(Some)
        .ok_or(AuthReadError::Invalid)
}

#[cfg(target_os = "macos")]
fn read_keychain_secret() -> Result<Option<String>, AuthReadError> {
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "gemini",
            "-a",
            "antigravity",
            "-w",
        ])
        .output()
        .map_err(|_| AuthReadError::Unavailable)?;
    if !output.status.success() {
        return Err(AuthReadError::Missing);
    }
    let raw = String::from_utf8(output.stdout).map_err(|_| AuthReadError::Invalid)?;
    let raw = raw.trim_matches(['\r', '\n']).trim();
    if raw.is_empty() {
        Ok(None)
    } else {
        Ok(Some(raw.to_owned()))
    }
}

#[cfg(not(target_os = "macos"))]
fn read_keychain_secret() -> Result<Option<String>, AuthReadError> {
    Err(AuthReadError::Unavailable)
}

fn parse_keychain_token(raw: &str) -> Option<KeychainToken> {
    let raw = raw.trim_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let text = if let Some(encoded) = raw.strip_prefix("go-keyring-base64:") {
        let bytes = STANDARD.decode(encoded.trim()).ok()?;
        String::from_utf8(bytes).ok()?
    } else {
        raw.to_owned()
    };
    let text = text.trim_matches(['\u{feff}', ' ', '\t', '\r', '\n']);

    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(object) = value.as_object() {
            return token_from_object(object);
        }
        if let Some(value) = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(KeychainToken {
                access_token: Some(value.to_owned()),
                refresh_token: None,
                expires_at_ms: None,
                account_email: None,
            });
        }
        return None;
    }

    if text.starts_with('{') || text.starts_with('[') {
        return None;
    }
    let token = text.strip_prefix("Bearer ").unwrap_or(text).trim();
    if token.is_empty() {
        return None;
    }
    Some(KeychainToken {
        access_token: Some(token.to_owned()),
        refresh_token: None,
        expires_at_ms: None,
        account_email: None,
    })
}

fn token_from_object(object: &serde_json::Map<String, Value>) -> Option<KeychainToken> {
    let source = object
        .get("token")
        .and_then(Value::as_object)
        .unwrap_or(object);
    let access_token = first_string(
        source,
        &[
            "access_token",
            "accessToken",
            "token",
            "id_token",
            "idToken",
            "bearerToken",
            "auth_token",
            "authToken",
        ],
    );
    let refresh_token = first_string(source, &["refresh_token", "refreshToken"]);
    let expires_at_ms =
        first_value(source, &["expiry", "expires_at", "expiresAt"]).and_then(parse_expiry);
    let id_token = first_string(source, &["id_token", "idToken"])
        .or_else(|| first_string(object, &["id_token", "idToken"]));
    let account_email = id_token.as_deref().and_then(parse_google_id_token_email);

    if access_token.is_some() || refresh_token.is_some() {
        return Some(KeychainToken {
            access_token,
            refresh_token,
            expires_at_ms,
            account_email,
        });
    }

    for key in ["tokens", "oauth", "oauth2", "credentials", "auth"] {
        if let Some(nested) = object.get(key).and_then(Value::as_object) {
            if let Some(token) = token_from_object(nested) {
                return Some(token);
            }
        }
    }
    None
}

// This email is display metadata from the Keychain ID token, not an authorization decision.
fn parse_google_id_token_email(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&payload).ok()?;
    if !matches!(
        claims.get("iss").and_then(Value::as_str),
        Some("https://accounts.google.com" | "accounts.google.com")
    ) || claims.get("email_verified").and_then(Value::as_bool) != Some(true)
    {
        return None;
    }
    claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_owned)
}

fn first_string(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn first_value<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a Value> {
    names.iter().find_map(|name| object.get(*name))
}

fn parse_expiry(value: &Value) -> Option<i64> {
    if let Some(value) = value.as_str() {
        return DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|time| time.timestamp_millis());
    }
    let value = value.as_f64().filter(|value| value.is_finite())?;
    let millis = if value.abs() >= 1_000_000_000_000.0 {
        value
    } else {
        value * 1_000.0
    };
    (i64::MIN as f64..=i64::MAX as f64)
        .contains(&millis)
        .then_some(millis as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_tokens_from_go_keyring_envelope_without_emitting_the_secret() {
        let credentials = json!({
            "token": {
                "access_token": "access-secret",
                "refresh_token": "refresh-secret",
                "expiry": "2026-09-23T10:00:00+02:00"
            }
        });
        let encoded = STANDARD.encode(serde_json::to_vec(&credentials).unwrap());
        let token = parse_keychain_token(&format!("go-keyring-base64:{encoded}")).unwrap();

        assert_eq!(token.access_token.as_deref(), Some("access-secret"));
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-secret"));
        assert_eq!(token.expires_at_ms, Some(1_790_150_400_000));
        assert_eq!(token.account_email, None);
    }

    #[test]
    fn reads_only_verified_google_email_claims_for_display() {
        let make_id_token = |claims: Value| {
            format!(
                "header.{}.signature",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
            )
        };
        let credentials = |id_token: String| json!({"id_token": id_token, "refresh_token": "fixture-refresh-token"});
        let id_token = make_id_token(json!({
            "iss": "https://accounts.google.com",
            "email": "antigravity.fixture@example.test",
            "email_verified": true
        }));
        let token = parse_keychain_token(&credentials(id_token).to_string()).unwrap();
        assert_eq!(
            token.account_email.as_deref(),
            Some("antigravity.fixture@example.test")
        );

        let unverified = make_id_token(json!({
            "iss": "https://accounts.google.com",
            "email": "antigravity.fixture@example.test",
            "email_verified": false
        }));
        assert_eq!(
            parse_keychain_token(&credentials(unverified).to_string())
                .unwrap()
                .account_email,
            None
        );

        let other_issuer = make_id_token(json!({
            "iss": "https://example.test",
            "email": "antigravity.fixture@example.test",
            "email_verified": true
        }));
        assert_eq!(
            parse_keychain_token(&credentials(other_issuer).to_string())
                .unwrap()
                .account_email,
            None
        );
    }

    #[test]
    fn reads_root_id_token_when_oauth_tokens_are_nested() {
        let id_token = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({
                    "iss": "https://accounts.google.com",
                    "email": "antigravity.fixture@example.test",
                    "email_verified": true
                }))
                .unwrap()
            )
        );
        let credentials = json!({
            "token": {
                "access_token": "fixture-access-token",
                "token_type": "Bearer",
                "refresh_token": "fixture-refresh-token",
                "expiry": "2026-09-23T10:00:00Z"
            },
            "auth_method": "fixture-auth-method",
            "id_token": id_token
        });

        assert_eq!(
            parse_keychain_token(&credentials.to_string())
                .unwrap()
                .account_email
                .as_deref(),
            Some("antigravity.fixture@example.test")
        );
    }

    #[test]
    fn accepts_bare_bearer_credentials_but_rejects_malformed_structured_data() {
        assert_eq!(
            parse_keychain_token("Bearer live-token")
                .unwrap()
                .access_token
                .as_deref(),
            Some("live-token")
        );
        assert!(parse_keychain_token("{not-json}").is_none());
    }
}
