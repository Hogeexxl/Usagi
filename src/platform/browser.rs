//! The small browser seam used by the launcher.

use std::{error::Error, fmt, net::SocketAddr};

pub const DASHBOARD_URL: &str = "http://127.0.0.1:3210";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserError(String);

impl BrowserError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for BrowserError {}

pub trait BrowserOpener: Send + Sync {
    fn open(&self, url: &str) -> Result<(), BrowserError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), BrowserError> {
        // Distribution/runtime tests set this seam so they can exercise the
        // launcher without opening the user's real browser.
        if std::env::var_os("USAGI_DISABLE_BROWSER").is_some() {
            return Ok(());
        }
        webbrowser::open(url).map_err(|error| BrowserError::new(error.to_string()))?;
        Ok(())
    }
}

pub fn dashboard_url(address: SocketAddr) -> String {
    format!("http://{address}")
}

pub fn open_dashboard_at<T: BrowserOpener + ?Sized>(
    opener: &T,
    address: SocketAddr,
) -> Result<(), BrowserError> {
    opener.open(&dashboard_url(address))
}

pub fn open_dashboard<T: BrowserOpener + ?Sized>(opener: &T) -> Result<(), BrowserError> {
    opener.open(DASHBOARD_URL)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingBrowser(Arc<Mutex<Vec<String>>>);

    impl BrowserOpener for RecordingBrowser {
        fn open(&self, url: &str) -> Result<(), BrowserError> {
            self.0.lock().unwrap().push(url.to_owned());
            Ok(())
        }
    }

    #[test]
    fn dashboard_open_at_uses_runtime_loopback_url() {
        let browser = RecordingBrowser::default();
        let address: SocketAddr = "127.0.0.1:3217".parse().unwrap();
        open_dashboard_at(&browser, address).unwrap();
        assert_eq!(
            browser.0.lock().unwrap().as_slice(),
            ["http://127.0.0.1:3217"]
        );
    }

    #[test]
    fn dashboard_open_uses_fixed_loopback_url() {
        let browser = RecordingBrowser::default();
        open_dashboard(&browser).unwrap();
        assert_eq!(browser.0.lock().unwrap().as_slice(), [DASHBOARD_URL]);
    }
}
