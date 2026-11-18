//! Keeps the Windows app current from the GitHub releases.
//!
//! At launch, and every few hours while the app is open, it looks up the latest release. If
//! that release is newer, it downloads `RainbowNest.exe`, checks it against the
//! release's `RainbowNest.exe.sha256`, and swaps it in for the running exe. The new version runs from the
//! next start; the toolbar offers a restart.
//!
//! Runs only in Windows release builds. Set `RAINBOWNEST_NO_UPDATE` to turn it off.

use crate::state::{Bus, Msg};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

const REPO: &str = "hussainph/nesting";
const ASSET: &str = "RainbowNest.exe";
const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);
/// Larger downloads are refused (the exe is about 32 MB).
const MAX_BYTES: u64 = 300 * 1024 * 1024;

pub fn enabled() -> bool {
    cfg!(all(windows, not(debug_assertions))) && std::env::var_os("RAINBOWNEST_NO_UPDATE").is_none()
        && std::env::var_os("NESTING_NO_UPDATE").is_none()
}

pub fn start(bus: Arc<Bus>) {
    if !enabled() {
        return;
    }
    std::thread::spawn(move || {
        let current = env!("CARGO_PKG_VERSION");
        loop {
            // Being offline isn't worth a message; a failed install is.
            if let Ok(Some(tag)) = latest_newer(current) {
                match install(&tag) {
                    Ok(()) => {
                        bus.send(Msg::Updated(version_of(&tag).to_string()));
                        return;
                    }
                    Err(e) => {
                        bus.send(Msg::UpdateFailed(version_of(&tag).to_string(), format!("{e:#}")));
                        return;
                    }
                }
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Starts the updated exe and quits this one.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe()
        && std::process::Command::new(exe).spawn().is_ok()
    {
        std::process::exit(0);
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent(concat!("rainbownest/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The latest release's tag, when it's newer than `current`.
pub fn latest_newer(current: &str) -> Result<Option<String>> {
    // releases/latest redirects to releases/tag/<tag>, which avoids the API's rate limit.
    let res = agent().head(format!("https://github.com/{REPO}/releases/latest")).call()?;
    let uri = ureq::ResponseExt::get_uri(&res).to_string();
    let tag = uri.rsplit('/').next().unwrap_or_default().to_string();
    Ok(is_newer(&tag, current).then_some(tag))
}

fn version_of(tag: &str) -> &str {
    tag.trim_start_matches(['v', 'V'])
}

fn is_newer(tag: &str, current: &str) -> bool {
    match (semver::Version::parse(version_of(tag)), semver::Version::parse(current)) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => false,
    }
}

fn download(url: &str) -> Result<Vec<u8>> {
    let res = agent().get(url).call().with_context(|| format!("Couldn't download {url}"))?;
    let mut bytes = Vec::new();
    res.into_body().into_reader().take(MAX_BYTES).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Downloads the release's exe, verifies it and puts it in place of the running one.
fn install(tag: &str) -> Result<()> {
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let exe = fetch_verified(&base)?;
    let current = std::env::current_exe()?;
    let staged = current.with_file_name(".RainbowNest-update.exe");
    std::fs::write(&staged, &exe).context("Couldn't save the update next to the app")?;
    let replaced = self_replace::self_replace(&staged).context("Couldn't replace the app's exe");
    let _ = std::fs::remove_file(&staged);
    replaced
}

/// The release's exe, after checking its SHA-256 and that it's a Windows program.
pub fn fetch_verified(base: &str) -> Result<Vec<u8>> {
    let sums = String::from_utf8(download(&format!("{base}/{ASSET}.sha256"))?)?;
    let expected = sums.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
    let exe = download(&format!("{base}/{ASSET}"))?;
    let actual: String = Sha256::digest(&exe).iter().map(|b| format!("{b:02x}")).collect();
    if expected.len() != 64 || actual != expected {
        bail!("The download didn't match its checksum");
    }
    if !exe.starts_with(b"MZ") {
        bail!("The download isn't a Windows program");
    }
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(!is_newer("latest", "0.1.1"));
        assert!(!is_newer("", "0.1.1"));
    }

    /// Talks to GitHub: `cargo test -p nest-app -- --ignored update`.
    #[test]
    #[ignore]
    fn update_finds_latest_release() {
        let tag = latest_newer("0.0.1").unwrap().expect("a release newer than 0.0.1");
        assert!(tag.starts_with('v'), "{tag}");
        assert_eq!(latest_newer("999.0.0").unwrap(), None);
    }

    /// Downloads a release's exe and checks it: `cargo test -p nest-app -- --ignored update`.
    /// Set `RAINBOWNEST_TEST_TAG` to pick the release (v0.1.3 or later).
    #[test]
    #[ignore]
    fn update_downloads_and_verifies() {
        let tag = std::env::var("RAINBOWNEST_TEST_TAG").unwrap_or_else(|_| "v0.1.3".into());
        let exe = fetch_verified(&format!("https://github.com/{REPO}/releases/download/{tag}")).unwrap();
        assert!(exe.len() > 1_000_000, "{} bytes", exe.len());
    }
}
