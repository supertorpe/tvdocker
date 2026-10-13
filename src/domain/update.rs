//! Release checking (parity row "Self-update" · §6 modal dialog ·
//! §12 "Help → Check for updates… + optional background startup check").
//!
//! Ported from tvdocker's `update.rs`. Two deliberate departures from the
//! upstream implementation, each pinned by its own test below: versions
//! compare **numerically** (upstream's string compare never reported an
//! update past component 10), and a newer release with no asset for this
//! target is still reported — pointing at the release page — rather than
//! being reported as "up to date".
//!
//! Downloading and replacing the running binary is deliberately NOT
//! implemented: the spec's user-facing promise is a *check* ("Check for
//! updates…"), no repository exists yet to download from, and swapping out
//! a running executable is destructive and untestable here.
use anyhow::Context as _;
use std::time::Duration;

/// Where releases are looked up.
///
/// **Placeholder.** The project has no GitHub repository yet —
/// calls this "set as a code constant when the repository is created",
/// which is exactly what this is. Swap the constant when the repo exists;
/// `the_release_url_names_the_placeholder_repository` pins it so the swap is
/// a deliberate, visible edit.
pub const REPOSITORY: &str = "https://github.com/tvdocker/tvdocker";

/// The human-facing releases page for [`REPOSITORY`] (what a reader is sent
/// to when there is no asset for this target).
pub fn latest_release_url() -> String {
    format!("{REPOSITORY}/releases/latest")
}

/// The endpoint [`check_for_update`] asks : GitHub's **API**, which
/// answers JSON — the human `/releases/latest` page answers HTML and would
/// never parse. Derived from [`REPOSITORY`] so swapping the placeholder for
/// a real repository swaps the endpoint too, and so `describe` can show
/// exactly what was asked.
pub fn release_api_url() -> String {
    let slug = REPOSITORY
        .strip_prefix("https://github.com/")
        .unwrap_or(REPOSITORY);
    format!("https://api.github.com/repos/{slug}/releases/latest")
}

/// The Rust target triple this build is for — used to pick the right asset.
///
/// Ports tvdocker's list; `"unknown"` for a platform with no published
/// build (the caller then falls back to the release page).
pub fn target_triple() -> &'static str {
    #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "musl"))]
    {
        "x86_64-unknown-linux-musl"
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "musl"))]
    {
        "aarch64-unknown-linux-musl"
    }
    #[cfg(all(target_arch = "arm", target_os = "linux", target_env = "gnu"))]
    {
        "armv7-unknown-linux-gnueabihf"
    }
    #[cfg(all(target_arch = "x86", target_os = "linux", target_env = "gnu"))]
    {
        "i686-unknown-linux-gnu"
    }
    #[cfg(all(target_arch = "x86", target_os = "linux", target_env = "musl"))]
    {
        "i686-unknown-linux-musl"
    }
    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_arch = "x86", target_os = "windows"))]
    {
        "i686-pc-windows-msvc"
    }
    #[cfg(not(any(
        all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"),
        all(target_arch = "x86_64", target_os = "linux", target_env = "musl"),
        all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"),
        all(target_arch = "aarch64", target_os = "linux", target_env = "musl"),
        all(target_arch = "arm", target_os = "linux", target_env = "gnu"),
        all(target_arch = "x86", target_os = "linux", target_env = "gnu"),
        all(target_arch = "x86", target_os = "linux", target_env = "musl"),
        all(target_arch = "x86_64", target_os = "macos"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "windows"),
        all(target_arch = "x86", target_os = "windows"),
    )))]
    {
        "unknown"
    }
}

/// The archive extension for this OS (the asset tvdocker publishes).
fn archive_ext() -> &'static str {
    if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    }
}

/// One release that is newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The tag with any leading `v` stripped.
    pub version: String,
    /// The asset for this target, or the release page when there is none.
    pub download_url: String,
}

/// What a check decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A newer release exists.
    Available(Release),
    /// Nothing newer than `current`.
    UpToDate,
    /// The repository has published no releases at all.
    NoReleaseYet,
}

/// Split `1.2.3-rc1` into its numeric run and its suffix.
///
/// A free fn, not a closure: the output borrows the input, which an inline
/// closure cannot name.
fn parts(v: &str) -> (Vec<&str>, &str) {
    match v.split_once('-') {
        Some((nums, suffix)) => (nums.split('.').collect(), suffix),
        None => (v.split('.').collect(), ""),
    }
}

/// Compare two versions, numerically per component, then per SemVer's
/// pre-release rule.
///
/// A longer run compares as greater (`0.10.0 > 0.9.0`); when the numeric
/// parts are equal, a **release beats a pre-release** (`1.0.0 > 1.0.0-rc1`
/// in either direction), and two suffixes compare byte-wise (`rc2 > rc1`).
fn is_newer(candidate: &str, current: &str) -> bool {
    let (a, a_suffix) = parts(candidate);
    let (b, b_suffix) = parts(current);
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(""),
            b.get(i).copied().unwrap_or(""),
        );
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) => nx.cmp(&ny),
            _ => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord == std::cmp::Ordering::Greater;
        }
    }
    match (a_suffix.is_empty(), b_suffix.is_empty()) {
        (true, false) => true,
        (false, true) => false,
        _ => a_suffix > b_suffix,
    }
}

/// Turn a `releases/latest` payload into a [`Release`] .
///
/// Pure — the network half lives in [`check_for_update`]. Returns `None`
/// when `tag_name` is not newer than the running version.
pub fn release_from(current: &str, json: &serde_json::Value) -> Option<Release> {
    let tag = json
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("v0.0.0");
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if !is_newer(version, current) {
        return None;
    }
    let page = json
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(REPOSITORY)
        .to_string();
    let wanted = format!("tvdocker_{version}_{}.{}", target_triple(), archive_ext());
    let asset = json
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                let same_name = a
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|n| n == wanted);
                if same_name {
                    a.get("browser_download_url")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                } else {
                    None
                }
            })
        });
    Some(Release {
        version: version.to_string(),
        download_url: asset.unwrap_or(page),
    })
}

/// Ask the configured repository for its latest release .
///
/// **Blocking** (ureq): call it from `spawn_blocking` in async contexts, or
/// from an explicit user action. Timeouts are short because the only caller
/// on the UI thread is `Help → Check for updates…`.
pub fn check_for_update(current: &str) -> anyhow::Result<Outcome> {
    check_for_update_at(&release_api_url(), current)
}

/// [`check_for_update`] against an explicit endpoint — the seam that lets
/// the transport path be exercised without a repository or a network round
/// trip (`an_unreachable_endpoint_is_reported_rather_than_swallowed`).
///
/// `204`/`404` mean "no releases published yet", which is not the same as
/// "up to date".
pub fn check_for_update_at(url: &str, current: &str) -> anyhow::Result<Outcome> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let resp = agent
        .get(url)
        .header("User-Agent", "tvdocker")
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("asking {url}"))?;
    let status = resp.status().as_u16();
    match status {
        // Read the body only where there is one: a body-read failure must
        // not turn a benign 404 ("nothing published yet") into a check
        // failure.
        204 | 404 => Ok(Outcome::NoReleaseYet),
        200 => {
            let body = resp.into_body().read_to_string()?;
            let json: serde_json::Value =
                serde_json::from_str(&body).context("reading the release payload")?;
            Ok(match release_from(current, &json) {
                Some(r) => Outcome::Available(r),
                None => Outcome::UpToDate,
            })
        }
        other => anyhow::bail!("HTTP {other} from {url}"),
    }
}

/// The startup gate (an **optional** background check). Only an
/// explicit `check_updates = true` starts one — an unset key stays silent.
pub fn check_on_startup(configured: Option<bool>) -> bool {
    configured.unwrap_or(false)
}

/// The `Help → Check for updates…` dialog body.
///
/// Total over the result: a failed check reports its cause rather than
/// looking like "up to date", and the endpoint that was asked is always
/// shown (the repository is a placeholder until one exists).
pub fn describe(current: &str, checked: &str, result: &anyhow::Result<Outcome>) -> String {
    match result {
        Ok(Outcome::Available(r)) => format!(
            "tvdocker {current} → {}.\n\nDownload:\n{}\n\nChecked:\n{checked}",
            r.version, r.download_url
        ),
        Ok(Outcome::UpToDate) => {
            format!("tvdocker {current} is up to date.\n\nChecked:\n{checked}")
        }
        Ok(Outcome::NoReleaseYet) => {
            format!("No releases are published for {REPOSITORY} yet.\n\nChecked:\n{checked}")
        }
        Err(e) => format!("Update check failed: {e:#}\n\nChecked:\n{checked}"),
    }
}

/// The status-latch text for the background check (background
/// outcomes go to the latch, never a modal). `None` means say nothing — a
/// placeholder repository that has published nothing is not news.
pub fn status_text(current: &str, result: &anyhow::Result<Outcome>) -> Option<String> {
    match result {
        Ok(Outcome::Available(r)) => Some(format!(
            "tvdocker {} is available — Help → Check for updates…",
            r.version
        )),
        Ok(Outcome::UpToDate) => Some(format!("tvdocker {current} is up to date")),
        Ok(Outcome::NoReleaseYet) => None,
        Err(e) => Some(format!("Update check failed: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `releases/latest` payload: tag, the download page, and optionally
    /// the asset that matches this build's target triple.
    fn payload(tag: &str, asset: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "tag_name": tag,
            "html_url": "https://github.com/tvdocker/tvdocker/releases/tag/placeholder",
            "assets": [],
        });
        if let Some(name) = asset {
            v["assets"] = serde_json::json!([
                {
                    "name": name,
                    "browser_download_url":
                        "https://github.com/tvdocker/tvdocker/releases/download/placeholder/x"
                }
            ]);
        }
        v
    }

    #[test]
    fn release_from_reports_a_newer_tag_and_its_matching_asset() {
        let want = format!("tvdocker_0.2.0_{}.{}", target_triple(), archive_ext());
        let out = release_from("0.1.0", &payload("v0.2.0", Some(&want)));
        let got = out.expect("0.2.0 > 0.1.0 is an update");
        assert_eq!(got.version, "0.2.0", "the leading v is stripped");
        assert!(
            got.download_url.contains("/releases/download/"),
            "the asset URL, not the tag page: {}",
            got.download_url
        );
    }

    #[test]
    fn release_from_keeps_quiet_when_current_is_not_older() {
        assert!(
            release_from("0.2.0", &payload("v0.2.0", None)).is_none(),
            "equal versions are up to date"
        );
        assert!(
            release_from("0.3.0", &payload("v0.2.0", None)).is_none(),
            "an older release is not news"
        );
        assert!(
            release_from("0.1.0", &payload("0.2.0", None)).is_some(),
            "an untagged v-prefixed-less release still parses"
        );
    }

    #[test]
    fn release_from_reports_a_newer_release_that_has_no_asset_for_this_target() {
        // Upstream returned `None` here and the dialog said "up to date"
        // while a newer release existed. Report it, pointing at the page.
        let got = release_from("0.1.0", &payload("v0.2.0", None)).expect("it IS newer");
        assert_eq!(got.version, "0.2.0");
        assert!(
            got.download_url.contains("/releases/"),
            "falls back to the release page: {}",
            got.download_url
        );
    }

    #[test]
    fn release_from_survives_a_payload_without_a_tag() {
        assert!(
            release_from("0.1.0", &serde_json::json!({"assets": []})).is_none(),
            "no tag ⇒ treated as v0.0.0 ⇒ not newer"
        );
    }

    #[test]
    fn versions_compare_numerically_not_lexicographically() {
        // tvdocker compared strings, so `0.10.0 <= 0.9.0` and every update
        // past component 10 was reported as "up to date".
        assert!(
            release_from("0.9.0", &payload("v0.10.0", None)).is_some(),
            "0.10.0 is newer than 0.9.0"
        );
        assert!(
            release_from("0.10.0", &payload("v0.9.0", None)).is_none(),
            "0.9.0 is older than 0.10.0"
        );
        assert!(
            release_from("0.9.9", &payload("v0.9.10", None)).is_some(),
            "and the third component counts too"
        );
    }

    #[test]
    fn the_release_url_names_the_placeholder_repository() {
        // No GitHub repo exists for this project yet (calls this an
        // implementation-time detail). This pins the placeholder so swapping
        // in the real repository is a deliberate, visible edit.
        assert_eq!(
            latest_release_url(),
            "https://github.com/tvdocker/tvdocker/releases/latest",
            "update REPOSITORY when the project gets a real repository"
        );
        assert_eq!(REPOSITORY, "https://github.com/tvdocker/tvdocker");
    }

    #[test]
    fn the_startup_check_is_opt_in() {
        assert!(check_on_startup(Some(true)), "enabled");
        assert!(!check_on_startup(None), "unset ⇒ silent");
        assert!(!check_on_startup(Some(false)), "explicitly off");
    }

    #[test]
    fn target_triple_matches_this_build() {
        #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
        assert_eq!(target_triple(), "x86_64-unknown-linux-gnu");
        assert_ne!(target_triple(), "unknown", "this platform is recognised");
    }

    #[test]
    fn the_dialog_names_the_release_and_where_to_get_it() {
        let rel = release_from("0.1.0", &payload("v0.2.0", None)).expect("newer");
        let text = describe("0.1.0", &latest_release_url(), &Ok(Outcome::Available(rel)));
        assert!(text.contains("0.2.0"), "the new version: {text}");
        assert!(text.contains("0.1.0"), "the running version: {text}");
        assert!(text.contains("/releases/"), "where to get it: {text}");
    }

    #[test]
    fn the_dialog_answers_up_to_date_and_no_releases_yet_honestly() {
        let ok = describe("0.1.0", &latest_release_url(), &Ok(Outcome::UpToDate));
        assert!(ok.contains("up to date"), "{ok}");
        assert!(ok.contains("0.1.0"), "{ok}");

        let none = describe("0.1.0", &latest_release_url(), &Ok(Outcome::NoReleaseYet));
        assert!(none.contains("No releases"), "{none}");
        assert!(
            none.contains("tvdocker/tvdocker"),
            "names the repository it asked: {none}"
        );
    }

    #[test]
    fn the_dialog_reports_a_failed_check_rather_than_swallowing_it() {
        let text = describe(
            "0.1.0",
            &latest_release_url(),
            &Err(anyhow::anyhow!("DNS failed")),
        );
        assert!(text.contains("failed"), "{text}");
        assert!(text.contains("DNS failed"), "the cause: {text}");
    }

    #[test]
    fn the_status_latch_only_speaks_when_it_has_something_to_say() {
        let rel = release_from("0.1.0", &payload("v0.2.0", None)).expect("newer");
        let available = status_text("0.1.0", &Ok(Outcome::Available(rel))).expect("news");
        assert!(available.contains("0.2.0"), "{available}");
        assert!(
            available.contains("Check for updates"),
            "actionable: {available}"
        );

        assert!(
            status_text("0.1.0", &Ok(Outcome::UpToDate)).is_some(),
            "the opt-in background check still confirms it ran"
        );
        assert!(
            status_text("0.1.0", &Ok(Outcome::NoReleaseYet)).is_none(),
            "a placeholder repository with no releases is not news"
        );
        let err = status_text("0.1.0", &Err(anyhow::anyhow!("timeout"))).expect("errors surface");
        assert!(err.contains("timeout"), "{err}");
    }

    /// GitHub serves the HUMAN release page (HTML) at `/releases/latest` and
    /// JSON at the API. Asking the web page makes every check fail to parse
    /// the moment `REPOSITORY` points at a repo that exists — it only looks
    /// correct today because the placeholder 404s before any parsing.
    #[test]
    fn the_release_endpoint_is_the_api_not_the_web_page() {
        assert_eq!(
            release_api_url(),
            "https://api.github.com/repos/tvdocker/tvdocker/releases/latest"
        );
        assert!(
            latest_release_url().starts_with("https://github.com/"),
            "the human-facing page stays on github.com: {}",
            latest_release_url()
        );
    }

    #[test]
    fn a_prerelease_is_not_an_update_over_its_final_release() {
        assert!(
            release_from("1.0.0", &payload("v1.0.0-rc1", None)).is_none(),
            "an rc is older than the release it precedes"
        );
        assert!(
            release_from("1.0.0-rc1", &payload("v1.0.0", None)).is_some(),
            "the final release beats the rc"
        );
        assert!(
            release_from("1.0.0-rc1", &payload("v1.0.0-rc2", None)).is_some(),
            "rc2 is newer than rc1"
        );
    }

    /// The transport path, pinned without a repository or a network: port 9
    /// (discard) on loopback refuses instantly, so a failed request must
    /// surface as an error — never as "up to date".
    #[test]
    fn an_unreachable_endpoint_is_reported_rather_than_swallowed() {
        let err = check_for_update_at("http://127.0.0.1:9/latest", "0.1.0")
            .expect_err("connection refused is an error, not 'up to date'");
        assert!(
            format!("{err:#}").contains("asking http://127.0.0.1:9/latest"),
            "the endpoint is named in the cause: {err:#}"
        );
    }

    /// The real network path — needs a repository that exists. Not run in CI
    /// (the project has no GitHub repo yet, so this would 404 by design).
    #[test]
    #[ignore = "needs a GitHub repository"]
    fn live_check_against_the_repository() {
        let out =
            check_for_update(env!("CARGO_PKG_VERSION")).expect("the request completes or errors");
        match out {
            Outcome::Available(r) => assert!(!r.version.is_empty()),
            Outcome::UpToDate | Outcome::NoReleaseYet => {}
        }
    }
}
