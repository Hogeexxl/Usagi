use chrono::DateTime;
use serde_json::Value;

use crate::codex::quota::CodexQuotaWindow;

const SESSION_WINDOW_SECONDS: u64 = 5 * 60 * 60;
const WEEKLY_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MappedSummary {
    pub(crate) session: Option<CodexQuotaWindow>,
    pub(crate) weekly: Option<CodexQuotaWindow>,
}

/// Parses only the shared Gemini pool IDs. Third-party buckets and future bucket IDs are ignored.
pub(crate) fn parse_summary(value: &Value) -> Option<MappedSummary> {
    let groups = value
        .get("response")
        .and_then(|response| response.get("groups"))
        .or_else(|| value.get("groups"))?
        .as_array()?;

    let mut session = None;
    let mut weekly = None;
    for bucket in groups
        .iter()
        .filter_map(|group| group.get("buckets").and_then(Value::as_array))
        .flatten()
    {
        let Some(bucket_id) = bucket.get("bucketId").and_then(Value::as_str) else {
            continue;
        };
        let target = match bucket_id {
            "gemini-5h" => &mut session,
            "gemini-weekly" => &mut weekly,
            _ => continue,
        };
        if target.is_some() {
            continue;
        }

        let Some(remaining_fraction) = bucket
            .get("remainingFraction")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        let period = if bucket_id == "gemini-5h" {
            SESSION_WINDOW_SECONDS
        } else {
            WEEKLY_WINDOW_SECONDS
        };
        *target = Some(map_window(
            remaining_fraction,
            bucket.get("resetTime").and_then(Value::as_str),
            period,
        ));
    }

    Some(MappedSummary { session, weekly })
}

pub(crate) fn parse_ls_plan(value: &Value) -> Option<String> {
    let status = value.get("userStatus")?;
    let raw = status
        .get("userTier")
        .and_then(|tier| tier.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            status
                .get("planStatus")
                .and_then(|plan| plan.get("planInfo"))
                .and_then(|info| info.get("planName"))
                .and_then(Value::as_str)
        });
    format_plan(raw)
}

pub(crate) fn parse_cloud_plan(value: &Value) -> Option<String> {
    let raw = value
        .get("paidTier")
        .and_then(|tier| tier.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("currentTier")
                .and_then(|tier| tier.get("name"))
                .and_then(Value::as_str)
        });
    format_plan(raw)
}

fn format_plan(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(tail) = raw.strip_prefix("Google AI ") {
        return title_words(tail);
    }
    for keyword in ["Ultra", "Pro", "Free"] {
        if raw.to_lowercase().contains(&keyword.to_lowercase()) {
            return Some(keyword.to_owned());
        }
    }
    title_words(raw)
}

fn title_words(value: &str) -> Option<String> {
    let words = value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            let first = chars.next()?;
            Some(first.to_uppercase().collect::<String>() + chars.as_str())
        })
        .collect::<Option<Vec<_>>>()?;
    (!words.is_empty()).then(|| words.join(" "))
}

fn map_window(fraction: f64, reset_time: Option<&str>, period: u64) -> CodexQuotaWindow {
    let remaining = fraction.clamp(0.0, 1.0);
    let remaining_percent = remaining * 100.0;
    let reset_at_ms = reset_time
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis());
    CodexQuotaWindow {
        used_percent: 100.0 - remaining_percent,
        remaining_percent,
        limit_window_seconds: period,
        reset_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_maps_exact_gemini_buckets_and_ignores_third_party_pools() {
        let value = json!({
            "response": {
                "groups": [{
                    "buckets": [
                        {"bucketId": "3p-5h", "remainingFraction": 0.0},
                        {"bucketId": "gemini-image-5h", "remainingFraction": 0.0},
                        {"bucketId": "gemini-5h", "remainingFraction": 0.25, "resetTime": "2026-09-23T08:00:00Z"},
                        {"bucketId": "gemini-weekly", "remainingFraction": 0.30056813, "resetTime": "invalid"},
                        {"bucketId": "3p-weekly", "remainingFraction": 0.0}
                    ]
                }]
            }
        });

        let mapped = parse_summary(&value).unwrap();
        assert_eq!(mapped.session.as_ref().unwrap().used_percent, 75.0);
        assert_eq!(
            mapped.session.as_ref().unwrap().limit_window_seconds,
            18_000
        );
        assert_eq!(
            mapped.session.as_ref().unwrap().reset_at_ms,
            Some(1_790_150_400_000)
        );
        assert!((mapped.weekly.as_ref().unwrap().used_percent - 69.943_187).abs() < 0.000_001);
        assert!((mapped.weekly.as_ref().unwrap().remaining_percent - 30.056_813).abs() < 0.000_001);
        assert_eq!(
            mapped.weekly.as_ref().unwrap().limit_window_seconds,
            604_800
        );
        assert_eq!(mapped.weekly.as_ref().unwrap().reset_at_ms, None);
    }

    #[test]
    fn summary_without_groups_is_not_authoritative_but_empty_groups_are() {
        assert!(parse_summary(&json!({"other": []})).is_none());
        assert_eq!(
            parse_summary(&json!({"groups": []})),
            Some(MappedSummary {
                session: None,
                weekly: None,
            })
        );
    }

    #[test]
    fn antigravity_plan_prefers_its_tier_over_the_inherited_plan() {
        assert_eq!(
            parse_ls_plan(&json!({
                "userStatus": {
                    "userTier": {"name": "Google AI Ultra"},
                    "planStatus": {"planInfo": {"planName": "Pro"}}
                }
            })),
            Some("Ultra".to_owned())
        );
        assert_eq!(
            parse_cloud_plan(&json!({
                "cloudaicompanionProject": {},
                "paidTier": {"id": "g1-pro-tier", "name": "Google AI Pro"},
                "currentTier": {"id": "free-tier", "name": "Free"},
                "allowedTiers": [{"id": "free-tier", "name": "Free", "isDefault": true}],
                "ineligibleTiers": []
            })),
            Some("Pro".to_owned())
        );
    }
}
