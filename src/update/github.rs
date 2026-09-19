//! GitHub's anonymous latest-release adapter.

use std::time::Duration;

use futures_util::{FutureExt, future::BoxFuture};
use reqwest::{Client, header};
use semver::Version;
use serde::Deserialize;

use super::{ReleaseInfo, ReleaseProvider};
use crate::update::state::{CURRENT_VERSION, UpdateFailureKind};

pub(crate) const GITHUB_OWNER: &str = "Hogeexxl";
pub(crate) const GITHUB_REPOSITORY: &str = "Usagi";
const GITHUB_API_VERSION: &str = "2022-11-28";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The production release source.  It carries no token and never sends any
/// local Codex or user information to GitHub.
#[derive(Clone)]
pub struct GithubReleaseAdapter {
    client: Client,
}

impl GithubReleaseAdapter {
    pub fn new() -> Result<Self, reqwest::Error> {
        let user_agent = format!("Usagi/{CURRENT_VERSION}");
        let client = Client::builder()
            .user_agent(user_agent)
            .connect_timeout(REQUEST_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self { client })
    }

    async fn fetch_latest_release(&self) -> Result<ReleaseInfo, UpdateFailureKind> {
        let response = self
            .latest_release_request()
            .send()
            .await
            .map_err(classify_request_error)?;

        let status = response.status();
        if !status.is_success() {
            return Err(UpdateFailureKind::HttpStatus(status.as_u16()));
        }

        let release = response
            .json::<GithubRelease>()
            .await
            .map_err(|_| UpdateFailureKind::InvalidJson)?;
        if release.draft || release.prerelease || release.published_at.is_none() {
            return Err(UpdateFailureKind::InvalidRelease);
        }

        ReleaseInfo::from_tag(&release.tag_name)
    }

    fn latest_release_request(&self) -> reqwest::RequestBuilder {
        latest_release_request(&self.client)
    }
}

impl ReleaseProvider for GithubReleaseAdapter {
    fn fetch_latest(&self) -> BoxFuture<'_, Result<ReleaseInfo, UpdateFailureKind>> {
        self.fetch_latest_release().boxed()
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
}

fn classify_request_error(error: reqwest::Error) -> UpdateFailureKind {
    if error.is_timeout() {
        UpdateFailureKind::Timeout
    } else if error.is_connect() {
        UpdateFailureKind::Network
    } else {
        UpdateFailureKind::Client
    }
}

fn latest_release_url() -> String {
    format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPOSITORY}/releases/latest")
}

fn latest_release_request(client: &Client) -> reqwest::RequestBuilder {
    client
        .get(latest_release_url())
        .header(header::USER_AGENT, format!("Usagi/{CURRENT_VERSION}"))
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
}

pub(crate) fn release_url_for_tag(tag: &str) -> Result<(Version, String), UpdateFailureKind> {
    let normalized = tag.strip_prefix('v').unwrap_or(tag);
    let version = Version::parse(normalized).map_err(|_| UpdateFailureKind::InvalidTag)?;
    if !version.pre.is_empty() || tag.is_empty() || !is_safe_tag(tag) {
        return Err(UpdateFailureKind::InvalidTag);
    }
    Ok((
        version,
        format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPOSITORY}/releases/tag/{tag}"),
    ))
}

fn is_safe_tag(tag: &str) -> bool {
    tag.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_repository_coordinates_are_single_source_of_truth() {
        assert_eq!(GITHUB_OWNER, "Hogeexxl");
        assert_eq!(GITHUB_REPOSITORY, "Usagi");
        assert_eq!(
            latest_release_url(),
            format!(
                "https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPOSITORY}/releases/latest"
            )
        );
    }

    #[test]
    fn latest_release_request_has_required_public_api_configuration() {
        let adapter = GithubReleaseAdapter::new().unwrap();
        let request = adapter.latest_release_request().build().unwrap();

        assert_eq!(
            request.url().as_str(),
            format!(
                "https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPOSITORY}/releases/latest"
            )
        );
        assert_eq!(
            request.headers().get(header::ACCEPT),
            Some(&header::HeaderValue::from_static(
                "application/vnd.github+json"
            ))
        );
        assert_eq!(
            request.headers().get("X-GitHub-Api-Version"),
            Some(&header::HeaderValue::from_static(GITHUB_API_VERSION))
        );
        assert_eq!(
            request.headers().get(header::USER_AGENT),
            Some(&header::HeaderValue::from_str(&format!("Usagi/{CURRENT_VERSION}")).unwrap())
        );
        assert!(request.headers().get(header::AUTHORIZATION).is_none());
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(5));
    }

    #[test]
    fn release_tags_accept_optional_v_and_reject_unstable_or_unsafe_values() {
        let (version, url) = release_url_for_tag("v0.1.1").unwrap();
        assert_eq!(version, Version::new(0, 1, 1));
        assert_eq!(
            url,
            format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPOSITORY}/releases/tag/v0.1.1")
        );

        assert_eq!(
            release_url_for_tag("0.1.1").unwrap().0,
            Version::new(0, 1, 1)
        );
        assert_eq!(
            release_url_for_tag("v0.1.1-beta").unwrap_err(),
            UpdateFailureKind::InvalidTag
        );
        assert_eq!(
            release_url_for_tag("v0.1.1/other").unwrap_err(),
            UpdateFailureKind::InvalidTag
        );
    }
}
