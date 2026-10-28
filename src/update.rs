//! Tells the user when a newer release is out; Homebrew only upgrades when asked.

use std::path::PathBuf;
use std::time::Duration;

const LATEST: &str = "https://github.com/Aayushkalikote/dropbridge/releases/latest";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Newest release version if it is newer than this build. GitHub redirects the "latest" page
/// to `.../releases/tag/vX.Y.Z`, which skips pre-releases and needs no API token.
pub async fn available() -> Option<String> {
    let out = tokio::process::Command::new("/usr/bin/curl")
        .args(["-fsS", "--max-time", "20", "-o", "/dev/null", "-w", "%{redirect_url}", LATEST])
        .output()
        .await
        .ok()?;
    let url = String::from_utf8(out.stdout).ok()?;
    let latest = url.trim().rsplit_once("/tag/v")?.1.to_string();
    newer(&latest, CURRENT).then_some(latest)
}

fn newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| v.split('.').map(|p| p.parse::<u32>().ok()).collect::<Option<Vec<_>>>();
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

/// Checks a minute after start, then daily; notifies once per new version (`seen` remembers it).
pub async fn watch(seen: PathBuf) {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(60), Duration::from_secs(24 * 3600));
    loop {
        tick.tick().await;
        let Some(v) = available().await else { continue };
        if std::fs::read_to_string(&seen).ok().as_deref() == Some(v.as_str()) {
            continue;
        }
        log::info!("DropBridge {v} is available (running {CURRENT})");
        crate::notify("DropBridge", &format!("Version {v} is available. Update: brew upgrade dropbridge")).await;
        let _ = std::fs::write(&seen, &v);
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("0.2.0-beta", "0.1.0"), "pre-releases are not offered");
        assert!(!newer("", "0.1.0"));
    }
}
