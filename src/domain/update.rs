//! Release checking and self-update (parity row "Self-update" · §6
//! modal dialog · §12 "Help → Check for updates… + optional background
//! startup check").
//!
//! Ported from tvdocker's `update.rs`. Two deliberate departures from the
//! upstream implementation, each pinned by its own test below: versions
//! compare **numerically** (upstream's string compare never reported an
//! update past component 10), and a newer release with no asset for this
//! target is still reported — pointing at the release page — rather than
//! being reported as "up to date".
//!
//! When the release HAS an asset for this target, the check dialog
//! becomes a Yes/No install offer: on Yes the task worker downloads the
//! archive, extracts it, and replaces the running binary (the
//! copy-to-`<exe>.new` + `rename` swap, same as oh-my-docker), then
//! reports through the status latch — restart tvdocker to apply.
use anyhow::Context as _;
use std::time::Duration;

/// Where releases are looked up and downloaded from.
///
/// Pinned by `the_release_url_names_the_repository` so changing it is a
/// deliberate, visible edit.
pub const REPOSITORY: &str = "https://github.com/supertorpe/tvdocker";

/// The human-facing releases page for [`REPOSITORY`] (what a reader is sent
/// to when there is no asset for this target).
pub fn latest_release_url() -> String {
    format!("{REPOSITORY}/releases/latest")
}

/// The endpoint [`check_for_update`] asks : GitHub's **API**, which
/// answers JSON — the human `/releases/latest` page answers HTML and would
/// never parse. Derived from [`REPOSITORY`] so it stays in one place, and
/// so `describe` can show exactly what was asked.
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
    /// The asset URL only when this target HAS a downloadable archive —
    /// `Some` is what makes the confirm-and-install flow possible; a
    /// page fallback leaves it `None`.
    pub asset: Option<String>,
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
        download_url: asset.clone().unwrap_or(page),
        asset,
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

/// The released binary's file name(s) inside an extracted archive.
fn binary_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["tvdocker.exe"]
    } else {
        &["tvdocker"]
    }
}

/// Depth-first search for the released binary in the extracted archive
/// tree (the release packs a staging dir around it).
pub fn find_binary(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_binary(&path) {
                return Some(found);
            }
        } else if path
            .file_name()
            .is_some_and(|n| binary_names().iter().any(|want| n == *want))
        {
            return Some(path);
        }
    }
    None
}

/// Blocking download of one release asset.
///
/// The connect timeout matches the check's, but the global timeout is
/// generous — the check's 5-second agent would abort a real archive
/// mid-flight. Transient 5xx/network failures retry three times before
/// giving up; the failure always names the URL.
pub fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut last = "connection refused".to_string();
    for attempt in 0..3 {
        match agent
            .get(url)
            .header("User-Agent", "tvdocker")
            .call()
            .map_err(|e| e.to_string())
        {
            Ok(resp) if resp.status().as_u16() < 500 => {
                return Ok(resp.into_body().read_to_vec()?);
            }
            Ok(resp) => last = format!("HTTP {}", resp.status()),
            Err(e) => last = e,
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    anyhow::bail!("{last} — download failed after 3 attempts: {url}")
}

/// Unpack the downloaded archive with the system `tar`/`unzip` (the
/// same tools oh-my-docker relies on; both ship everywhere the
/// releases target).
fn extract_archive(archive: &std::path::Path, dest: &std::path::Path) -> anyhow::Result<()> {
    let status = if archive.extension().is_some_and(|e| e == "zip") {
        std::process::Command::new("unzip")
            .args([
                "-o",
                &archive.to_string_lossy(),
                "-d",
                &dest.to_string_lossy(),
            ])
            .status()
    } else {
        std::process::Command::new("tar")
            .args([
                "-xzf",
                &archive.to_string_lossy(),
                "-C",
                &dest.to_string_lossy(),
            ])
            .status()
    }?;
    if !status.success() {
        anyhow::bail!("archive extraction failed");
    }
    Ok(())
}

/// Download the release for `version` and replace the running binary.
pub fn perform_update(version: &str, url: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locating the running binary")?;
    perform_update_into(&exe, &std::env::temp_dir(), version, url, download)
}

/// [`perform_update`] with the environment-dependent halves injected:
/// the binary to replace, the scratch root, and the fetch. The swap
/// itself is the copy-to-`<exe>.new` + `rename` idiom — atomic on Linux
/// even while the binary runs (the old inode lives until the process
/// exits); on Windows the rename fails and the error surfaces, same as
/// oh-my-docker.
pub fn perform_update_into(
    current_exe: &std::path::Path,
    temp_root: &std::path::Path,
    version: &str,
    url: &str,
    fetch: impl Fn(&str) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temp_dir = temp_root.join(format!("tvdocker_update_{version}_{unique}"));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir)?;
    let ext = if url.ends_with(".zip") {
        "zip"
    } else {
        "tar.gz"
    };
    let archive = temp_dir.join(format!("archive.{ext}"));

    let data = fetch(url).with_context(|| format!("downloading {url}"))?;
    std::fs::write(&archive, &data).context("staging the archive")?;
    let extracted = temp_dir.join("extracted");
    std::fs::create_dir_all(&extracted)?;
    extract_archive(&archive, &extracted).context("extracting the release archive")?;

    let binary = find_binary(&extracted)
        .ok_or_else(|| anyhow::anyhow!("tvdocker binary not found in the archive"))?;
    let staged = current_exe.with_extension("new");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(&binary, &staged).context("staging the new binary")?;
    std::fs::rename(&staged, current_exe).context("replacing the running binary")?;
    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// The `Help → Check for updates…` dialog body.
///
/// Total over the result: a failed check reports its cause rather than
/// looking like "up to date", and the endpoint that was asked is always
/// shown.
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
/// repository that has published nothing is not news.
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

/// The Yes/No question shown when the check found a self-updatable
/// release — consent names both versions and the restart consequence.
pub fn describe_install_offer(current: &str, version: &str) -> String {
    format!(
        "tvdocker {current} → {version}.\n\n\
         Download and install this update now? tvdocker will be \
         replaced on disk and must be restarted to apply it."
    )
}

/// The status-latch text after a successful install.
pub fn install_done_text(version: &str) -> String {
    format!("Updated to tvdocker {version} — restart tvdocker to apply")
}

/// The latched error for a failed install.
pub fn install_failed_text(e: &anyhow::Error) -> String {
    format!("Update failed: {e:#}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `releases/latest` payload: tag, the download page, and optionally
    /// the asset that matches this build's target triple.
    fn payload(tag: &str, asset: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "tag_name": tag,
            "html_url": "https://github.com/supertorpe/tvdocker/releases/tag/placeholder",
            "assets": [],
        });
        if let Some(name) = asset {
            v["assets"] = serde_json::json!([
                {
                    "name": name,
                    "browser_download_url":
                        "https://github.com/supertorpe/tvdocker/releases/download/placeholder/x"
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
        assert_eq!(got.asset, Some(got.download_url.clone()), "self-updatable");
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
        assert!(
            got.asset.is_none(),
            "no asset for this target ⇒ not self-updatable"
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
    fn the_release_url_names_the_repository() {
        // Changing where releases are looked up is a deliberate, visible
        // edit — this pins the exact constant.
        assert_eq!(
            latest_release_url(),
            "https://github.com/supertorpe/tvdocker/releases/latest",
            "update REPOSITORY deliberately if the project ever moves"
        );
        assert_eq!(REPOSITORY, "https://github.com/supertorpe/tvdocker");
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
            none.contains("supertorpe/tvdocker"),
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
            "no releases is not news"
        );
        let err = status_text("0.1.0", &Err(anyhow::anyhow!("timeout"))).expect("errors surface");
        assert!(err.contains("timeout"), "{err}");
    }

    /// GitHub serves the HUMAN release page (HTML) at `/releases/latest` and
    /// JSON at the API. Asking the web page would make every check fail to
    /// parse; the JSON endpoint is the contract this code relies on.
    #[test]
    fn the_release_endpoint_is_the_api_not_the_web_page() {
        assert_eq!(
            release_api_url(),
            "https://api.github.com/repos/supertorpe/tvdocker/releases/latest"
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

    /// The real network path. Kept ignored so the suite stays hermetic;
    /// run it locally with `cargo test -- --ignored`.
    #[test]
    #[ignore = "hits the network"]
    fn live_check_against_the_repository() {
        let out =
            check_for_update(env!("CARGO_PKG_VERSION")).expect("the request completes or errors");
        match out {
            Outcome::Available(r) => assert!(!r.version.is_empty()),
            Outcome::UpToDate | Outcome::NoReleaseYet => {}
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tvdocker-fb-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn find_binary_locates_the_release_binary_in_the_extracted_tree() {
        // The archive holds a staging dir (`tvdocker_{version}_{target}/`)
        // with the binary inside it — one level deep.
        let root = scratch("nested");
        let staged = root.join("tvdocker_0.2.0_x86_64-unknown-linux-gnu");
        std::fs::create_dir_all(&staged).expect("staging dir");
        std::fs::write(staged.join("tvdocker"), b"bin").expect("binary");
        std::fs::write(staged.join("README.md"), b"# tvdocker").expect("readme");
        assert_eq!(
            find_binary(&root).as_deref(),
            Some(staged.join("tvdocker").as_path()),
            "the binary, not the readme beside it"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn find_binary_returns_none_when_the_archive_has_no_binary() {
        let root = scratch("empty");
        std::fs::write(root.join("README.md"), b"# tvdocker").expect("readme");
        assert_eq!(find_binary(&root), None, "no binary to install");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Build a release-shaped archive (staging dir + binary inside) and
    /// return its bytes — the shape `.github/workflows` publishes.
    #[cfg(unix)]
    fn release_archive(inner: &str, binary: Option<&[u8]>) -> Vec<u8> {
        let stage = scratch(&format!("stage-{inner}"));
        let tree = stage.join(format!("tvdocker_9.9.9_{}", target_triple()));
        std::fs::create_dir_all(&tree).expect("staging dir");
        std::fs::write(tree.join("README.md"), inner).expect("readme");
        if let Some(bytes) = binary {
            std::fs::write(tree.join("tvdocker"), bytes).expect("binary");
        }
        let archive = stage.join("archive.tar.gz");
        let status = std::process::Command::new("tar")
            .args([
                "-czf",
                archive.to_str().unwrap(),
                "-C",
                stage.to_str().unwrap(),
                tree.file_name().unwrap().to_str().unwrap(),
            ])
            .status()
            .expect("tar runs");
        assert!(status.success(), "tar packs the staging dir");
        let bytes = std::fs::read(&archive).expect("archive bytes");
        std::fs::remove_dir_all(&stage).ok();
        bytes
    }

    #[cfg(unix)]
    #[test]
    fn perform_update_downloads_extracts_and_swaps_the_running_binary() {
        let root = scratch("swap");
        let exe = root.join("tvdocker");
        std::fs::write(&exe, b"OLD").expect("running binary");
        let bytes = release_archive("staged", Some(b"NEW"));
        perform_update_into(
            &exe,
            &root,
            "9.9.9",
            "http://example/archive.tar.gz",
            |_| Ok(bytes.clone()),
        )
        .expect("the update installs");
        assert_eq!(
            std::fs::read(&exe).expect("exe"),
            b"NEW",
            "the running binary is the new one"
        );
        let left: Vec<_> = std::fs::read_dir(&root)
            .expect("root readable")
            .map(|e| e.expect("entry").path())
            .collect();
        assert_eq!(left, vec![exe.clone()], "only the swapped binary remains");
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_download_leaves_the_running_binary_untouched() {
        let root = scratch("dl-fail");
        let exe = root.join("tvdocker");
        std::fs::write(&exe, b"OLD").expect("running binary");
        let err = perform_update_into(&exe, &root, "9.9.9", "http://example/a.tar.gz", |_| {
            Err(anyhow::anyhow!("connection refused"))
        })
        .expect_err("a failed download is an error");
        assert!(
            format!("{err:#}").contains("http://example/a.tar.gz"),
            "the URL is named in the cause: {err:#}"
        );
        assert_eq!(std::fs::read(&exe).expect("exe"), b"OLD", "untouched");
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn an_archive_without_the_binary_installs_nothing() {
        let root = scratch("no-bin");
        let exe = root.join("tvdocker");
        std::fs::write(&exe, b"OLD").expect("running binary");
        let bytes = release_archive("readme only", None);
        let err = perform_update_into(&exe, &root, "9.9.9", "http://example/a.tar.gz", |_| {
            Ok(bytes.clone())
        })
        .expect_err("no binary to install");
        assert!(
            format!("{err:#}").contains("binary not found"),
            "the cause says why: {err:#}"
        );
        assert_eq!(std::fs::read(&exe).expect("exe"), b"OLD", "untouched");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The transport path, pinned without a repository: port 9 (discard)
    /// on loopback refuses instantly, so the failure must surface — with
    /// the URL — instead of hanging or returning empty bytes.
    #[test]
    fn download_reports_failure_rather_than_hanging() {
        let err = download("http://127.0.0.1:9/archive.tar.gz")
            .expect_err("connection refused is an error");
        assert!(
            format!("{err:#}").contains("http://127.0.0.1:9/archive.tar.gz"),
            "the URL is named: {err:#}"
        );
    }

    #[test]
    fn the_install_offer_names_both_versions_and_asks_for_consent() {
        let text = describe_install_offer("0.1.0", "0.2.0");
        assert!(text.contains("0.1.0"), "the running version: {text}");
        assert!(text.contains("0.2.0"), "the new version: {text}");
        assert!(
            text.contains('?'),
            "it is a question, not a statement: {text}"
        );
        assert!(
            text.to_lowercase().contains("restart"),
            "consent is informed — the swap needs a restart: {text}"
        );
    }

    #[test]
    fn the_install_result_names_the_version_and_the_restart() {
        let done = install_done_text("0.2.0");
        assert!(done.contains("0.2.0"), "{done}");
        assert!(done.to_lowercase().contains("restart"), "{done}");

        let failed = install_failed_text(&anyhow::anyhow!("connection refused"));
        assert!(failed.contains("failed"), "{failed}");
        assert!(failed.contains("connection refused"), "{failed}");
    }
}
