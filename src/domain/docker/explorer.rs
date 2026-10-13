//! Container/volume/host file browsing ("Explorer ×N";
//! vendored from oh-my-docker's `docker/explorer.rs`, adapted to the
//! TaskReq worker: the listing fns are plain `async`/blocking fns the
//! worker drives).

use anyhow::Result;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogOutput, StartContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::HostConfig;
use futures_util::StreamExt;

/// One directory row (vendored field-for-field from tvdocker's
/// `ExplorerEntry`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExplorerEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub modified: String,
    pub permissions: String,
}

/// `ls -l` mode string (vendored tvdocker `format_mode`, extended to
/// render the symlink file-type bit as `l`).
pub fn format_mode(mode: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(if mode & 0o170000 == 0o120000 {
        'l'
    } else if mode & 0o040000 != 0 {
        'd'
    } else {
        '-'
    });
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o001 != 0 { 'x' } else { '-' });
    s
}

/// Parse one `ls -lap` line (vendored tvdocker `parse_ls_line`): the
/// first field must be a mode string; the name is fields 7+ rejoined
/// (names may contain spaces); ` -> target` suffixes are stripped;
/// `.`/`..`/blanks/`total` lines are dropped.
pub fn parse_ls_line(line: &str) -> Option<ExplorerEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("total ") {
        return None;
    }
    let bytes = line.as_bytes();
    let first = bytes.first().copied()?;
    if first != b'd' && first != b'-' && first != b'l' {
        return None;
    }
    let perms_end = bytes.iter().position(|b| *b == b' ').unwrap_or(bytes.len());
    let perms = &line[..perms_end];
    let is_dir = perms.starts_with('d');
    let rest = line[perms_end..].trim_start();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 8 {
        return None;
    }
    let size: i64 = fields[3].parse().unwrap_or(0);
    let modified = format!("{} {} {}", fields[4], fields[5], fields[6]);
    let mut name = fields[7..].join(" ");
    if let Some(pos) = name.find(" -> ") {
        name.truncate(pos);
    }
    let name = name.strip_suffix('/').unwrap_or(&name).to_string();
    if name == "." || name == ".." {
        return None;
    }
    Some(ExplorerEntry {
        name,
        is_dir,
        size,
        modified,
        permissions: perms.to_string(),
    })
}

/// Dirs first, then case-insensitive name (upstream
/// tvdocker orders dirs last, but an explorer shows directories first).
pub fn sort_entries(entries: &mut [ExplorerEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// List a host directory (BLOCKING std::fs — the worker wraps it in
/// `spawn_blocking`; never call it on the UI thread).
pub fn list_host_dir(path: &str) -> Result<Vec<ExplorerEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        let size = meta.as_ref().map_or(0, |m| m.len() as i64);
        let modified = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%b %e %H:%M").to_string()
            })
            .unwrap_or_default();
        let permissions = meta.as_ref().map_or_else(String::new, |m| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                format_mode(m.permissions().mode() & 0o7777)
            }
            #[cfg(not(unix))]
            {
                let _ = m;
                String::new()
            }
        });
        entries.push(ExplorerEntry {
            name,
            is_dir,
            size,
            modified,
            permissions,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// The helper container mounts a volume at this path (vendored).
const VOLUME_HELPER_IMAGE: &str = "alpine:latest";
const VOLUME_MOUNT_PATH: &str = "/volume_data";

fn volume_container_name(volume_name: &str) -> String {
    format!("tvdocker-vol-{volume_name}")
}

/// Map a browsed path into the helper container's mount (vendored).
pub fn volume_container_path(path: &str) -> String {
    if path == "/" {
        VOLUME_MOUNT_PATH.to_string()
    } else {
        format!("{}{}", VOLUME_MOUNT_PATH, path.trim_end_matches('/'))
    }
}

/// Drain an attached exec's output, returning (stdout, stderr) as text
/// (vendored shape extended: listings use stdout, mutations fail on a
/// non-empty stderr — `rm`/`touch`/`mv` report failures there).
async fn exec_collect(
    docker: &Docker,
    container: &str,
    cmd: Vec<String>,
) -> Result<(String, String)> {
    let exec = docker
        .create_exec(
            container,
            CreateExecOptions {
                cmd: Some(cmd),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await?;
    let output = docker
        .start_exec(&exec.id, None::<StartExecOptions>)
        .await?;
    let mut stdout = Vec::new();
    let mut stderr = String::new();
    if let StartExecResults::Attached { mut output, .. } = output {
        while let Some(msg) = output.next().await {
            match msg {
                Ok(LogOutput::StdOut { message }) => stdout.extend_from_slice(&message),
                Ok(LogOutput::StdErr { message }) => {
                    stderr.push_str(&String::from_utf8_lossy(&message));
                }
                _ => {}
            }
        }
    }
    Ok((String::from_utf8_lossy(&stdout).to_string(), stderr))
}

/// `ls -lap` a container directory (vendored, via the shared exec
/// helper). A non-empty stderr is reported through the parse: listing
/// errors surface as an empty-but-error result only when create/exec
/// fails — matching upstream's "stdout may be partial" stance.
pub async fn list_container_dir(
    docker: &Docker,
    container_id: &str,
    path: &str,
) -> Result<Vec<ExplorerEntry>> {
    let (stdout, _err) = exec_collect(
        docker,
        container_id,
        vec!["ls".into(), "-lap".into(), "--".into(), path.into()],
    )
    .await?;
    let mut entries: Vec<ExplorerEntry> = stdout.lines().filter_map(parse_ls_line).collect();
    sort_entries(&mut entries);
    Ok(entries)
}

/// Prefix for the ephemeral containers that make an image browsable
/// (image-explorer).
const IMAGE_HELPER_PREFIX: &str = "tvdocker-explore";

/// Long enough to outlive any session; `auto_remove` reaps the helper the
/// moment it stops.
const IMAGE_HELPER_SLEEP_SECS: &str = "86400";

/// The helper's container name for one image (image-explorer).
/// Deterministic, so reopening the same image reuses the same helper;
/// Docker's name regex is `[a-zA-Z0-9][a-zA-Z0-9_.-]*`, so the `:` in a
/// `sha256:` id becomes `-`.
pub fn image_container_name(image_id: &str) -> String {
    format!("{IMAGE_HELPER_PREFIX}-{}", image_id.replace(':', "-"))
}

/// The create-config for one image's helper (pure, so it can be unit-tested
/// without a daemon).
pub fn image_helper_config(image_id: &str) -> Config<String> {
    Config::<String> {
        image: Some(image_id.to_string()),
        cmd: Some(vec!["sleep".into(), IMAGE_HELPER_SLEEP_SECS.into()]),
        // LOAD-BEARING: Docker prepends the image's Entrypoint to the Cmd we
        // pass, so an nginx image would start `/docker-entrypoint.sh sleep
        // 86400` instead of sleeping (the volume helper sidesteps this only
        // because it pins `alpine:latest`, which has no entrypoint). An empty
        // override makes `sleep` the whole command.
        entrypoint: Some(vec![]),
        host_config: Some(HostConfig {
            auto_remove: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Ensure the ephemeral helper for `image_id` exists and runs; return its id
/// (the `ensure_volume_helper` pattern,). HOST-MUTATING.
pub async fn ensure_image_helper(docker: &Docker, image_id: &str) -> Result<String> {
    let name = image_container_name(image_id);
    let existing = docker
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            filters: [("name".to_string(), vec![name.clone()])]
                .into_iter()
                .collect(),
            ..Default::default()
        }))
        .await?
        .into_iter()
        .next();
    if let Some(c) = existing
        && let Some(id) = c.id
    {
        if c.state.as_deref() != Some("running") {
            docker
                .start_container(&id, None::<StartContainerOptions<String>>)
                .await?;
        }
        return Ok(id);
    }
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: name.as_str(),
                platform: None,
            }),
            image_helper_config(image_id),
        )
        .await?;
    docker
        .start_container(&container.id, None::<StartContainerOptions<String>>)
        .await?;
    Ok(container.id)
}

/// Stop the helper (auto-remove deletes it; a 404 is success — the
/// `remove_volume_helper` pattern). HOST-MUTATING.
pub async fn remove_image_helper(docker: &Docker, image_id: &str) -> Result<()> {
    let name = image_container_name(image_id);
    match docker
        .stop_container(&name, None::<StopContainerOptions>)
        .await
    {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().contains("404") || e.to_string().contains("not found") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// List one image directory: resolve the helper, then reuse the container
/// listing. NO path mapping — the helper runs the image itself, so an
/// absolute browsed path already IS the image's path (a volume helper needs
/// `volume_container_path` only because it mounts the volume elsewhere).
pub async fn list_image_dir(
    docker: &Docker,
    image_id: &str,
    path: &str,
) -> Result<Vec<ExplorerEntry>> {
    let id = ensure_image_helper(docker, image_id).await?;
    list_container_dir(docker, &id, path).await
}

/// Ensure the ephemeral volume helper exists and runs; return its id
/// (vendored tvdocker, renamed `tvdocker-vol-*`). HOST-MUTATING.
pub async fn ensure_volume_helper(docker: &Docker, volume_name: &str) -> Result<String> {
    let name = volume_container_name(volume_name);
    let existing = docker
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            filters: [("name".to_string(), vec![name.clone()])]
                .into_iter()
                .collect(),
            ..Default::default()
        }))
        .await?
        .into_iter()
        .next();
    if let Some(c) = existing
        && let Some(id) = c.id
    {
        if c.state.as_deref() != Some("running") {
            docker
                .start_container(&id, None::<StartContainerOptions<String>>)
                .await?;
        }
        return Ok(id);
    }
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: name.as_str(),
                platform: None,
            }),
            Config::<String> {
                image: Some(VOLUME_HELPER_IMAGE.into()),
                cmd: Some(vec!["sleep".into(), "86400".into()]),
                host_config: Some(HostConfig {
                    binds: Some(vec![format!("{volume_name}:{VOLUME_MOUNT_PATH}:z")]),
                    auto_remove: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    docker
        .start_container(&container.id, None::<StartContainerOptions<String>>)
        .await?;
    Ok(container.id)
}

/// Stop the helper (auto-remove deletes it; a 404 is success —
/// vendored). HOST-MUTATING.
pub async fn remove_volume_helper(docker: &Docker, volume_name: &str) -> Result<()> {
    let name = volume_container_name(volume_name);
    match docker
        .stop_container(&name, None::<StopContainerOptions>)
        .await
    {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().contains("404") || e.to_string().contains("not found") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// List a volume directory through the helper (vendored: ensure → map
/// → exec ls). HOST-MUTATING via `ensure_volume_helper`.
pub async fn list_volume_dir(
    docker: &Docker,
    volume_name: &str,
    path: &str,
) -> Result<Vec<ExplorerEntry>> {
    let id = ensure_volume_helper(docker, volume_name).await?;
    list_container_dir(docker, &id, &volume_container_path(path)).await
}

/// Build the upload archive for the given host paths (vendored tvdocker
/// `copy_to_container`'s tar half, extracted so it is unit-testable):
/// directories are appended recursively, files individually.
pub fn tar_bytes(host_paths: &[String]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        for spec in host_paths {
            let path = std::path::Path::new(spec);
            if !path.exists() {
                return Err(anyhow::anyhow!("Path does not exist: {spec}"));
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow::anyhow!("Invalid file name: {spec}"))?;
            if path.is_dir() {
                builder.append_dir_all(name, path)?;
            } else {
                let mut file = std::fs::File::open(path)?;
                builder.append_file(name, &mut file)?;
            }
        }
        builder.finish()?;
    }
    Ok(bytes)
}

/// Upload the host paths into `container_dst` (vendored; multi-path
/// support is the one adaptation — upstream copied one path per call).
pub async fn copy_to_container(
    docker: &Docker,
    container_id: &str,
    host_paths: &[String],
    container_dst: &str,
) -> Result<usize> {
    if host_paths.is_empty() {
        return Ok(0);
    }
    let count = host_paths.len();
    let bytes = tar_bytes(host_paths)?;
    docker
        .upload_to_container(
            container_id,
            Some(bollard::container::UploadToContainerOptions {
                path: container_dst.to_string(),
                no_overwrite_dir_non_dir: "false".to_string(),
            }),
            bytes.into(),
        )
        .await?;
    Ok(count)
}

/// Download the container paths into the host dir (vendored: the daemon
/// streams a tar; we unpack it).
pub async fn copy_from_container(
    docker: &Docker,
    container_id: &str,
    container_paths: &[String],
    host_dst_dir: &str,
) -> Result<usize> {
    if container_paths.is_empty() {
        return Ok(0);
    }
    let mut all_bytes = Vec::new();
    for p in container_paths {
        let mut stream = docker.download_from_container(
            container_id,
            Some(bollard::container::DownloadFromContainerOptions { path: p.clone() }),
        );
        while let Some(chunk) = stream.next().await {
            all_bytes.extend_from_slice(&chunk?);
        }
    }
    tar::Archive::new(all_bytes.as_slice()).unpack(host_dst_dir)?;
    Ok(container_paths.len())
}

/// Reject a mutation when the exec command wrote to stderr (`rm`,
/// `touch` and `mv` surface failures there while stdout stays empty).
fn reject_stderr(err: &str) -> Result<()> {
    if err.trim().is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{}", err.trim()))
    }
}

/// `rm -rf -- p1 p2 …` in a container (vendored shape; multi-path is
/// the adaptation). A non-empty stderr fails the call.
pub async fn delete_in_container(
    docker: &Docker,
    container_id: &str,
    paths: &[String],
) -> Result<()> {
    let mut cmd = vec!["rm".to_string(), "-rf".to_string(), "--".to_string()];
    cmd.extend(paths.iter().cloned());
    let (_out, err) = exec_collect(docker, container_id, cmd).await?;
    reject_stderr(&err)
}

/// Delete host paths (BLOCKING fs; the worker wraps it).
pub fn delete_host_paths(paths: &[String]) -> Result<()> {
    for p in paths {
        let path = std::path::Path::new(p);
        if path.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// `mkdir -p` / `touch` in a container (vendored).
pub async fn create_in_container(
    docker: &Docker,
    container_id: &str,
    path: &str,
    is_dir: bool,
) -> Result<()> {
    let cmd = if is_dir {
        vec![
            "mkdir".to_string(),
            "-p".to_string(),
            "--".to_string(),
            path.to_string(),
        ]
    } else {
        vec!["touch".to_string(), "--".to_string(), path.to_string()]
    };
    let (_out, err) = exec_collect(docker, container_id, cmd).await?;
    reject_stderr(&err)
}

/// Create a host dir/file (BLOCKING fs).
pub fn create_host_path(path: &str, is_dir: bool) -> Result<()> {
    if is_dir {
        std::fs::create_dir_all(path)?;
    } else {
        std::fs::write(path, [])?;
    }
    Ok(())
}

/// `mv -- old new` in a container (vendored).
pub async fn rename_in_container(
    docker: &Docker,
    container_id: &str,
    old: &str,
    new: &str,
) -> Result<()> {
    let (_out, err) = exec_collect(
        docker,
        container_id,
        vec!["mv".into(), "--".into(), old.into(), new.into()],
    )
    .await?;
    reject_stderr(&err)
}

/// Rename a host path (BLOCKING fs).
pub fn rename_host_path(old: &str, new: &str) -> Result<()> {
    std::fs::rename(old, new)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mode_renders_unix_permissions() {
        assert_eq!(format_mode(0o040755), "drwxr-xr-x");
        assert_eq!(format_mode(0o100644), "-rw-r--r--");
        assert_eq!(format_mode(0o120777), "lrwxrwxrwx");
    }

    #[test]
    fn parse_ls_line_reads_a_normal_entry() {
        let e =
            parse_ls_line("-rw-r--r--   1 root root   1234 Oct 11 09:30 notes.txt").expect("entry");
        assert_eq!(
            e,
            ExplorerEntry {
                name: "notes.txt".into(),
                is_dir: false,
                size: 1234,
                modified: "Oct 11 09:30".into(),
                permissions: "-rw-r--r--".into(),
            }
        );
    }

    #[test]
    fn parse_ls_line_handles_dirs_links_noise_and_names_with_spaces() {
        let dir = parse_ls_line("drwxr-xr-x 2 root root 4096 Oct 11 09:00 app").expect("dir");
        assert!(dir.is_dir);
        // Symlink arrows are stripped (vendored).
        let link =
            parse_ls_line("lrwxrwxrwx 1 root root 8 Oct 11 09:00 lib -> /usr/lib").expect("link");
        assert_eq!(link.name, "lib");
        assert!(
            !link.is_dir,
            "the leading l still marks non-dirs; only d is a dir"
        );
        // Names with spaces survive (fields 7+ are rejoined).
        let spaced =
            parse_ls_line("-rw-r--r-- 1 root root 9 Oct 11 09:00 my file.txt").expect("spaced");
        assert_eq!(spaced.name, "my file.txt");
        // Noise: blank lines, `total`, and non-file lines are skipped.
        assert!(parse_ls_line("").is_none());
        assert!(parse_ls_line("total 24").is_none());
        assert!(parse_ls_line("app:").is_none());
        assert!(parse_ls_line(".").is_none());
        assert!(parse_ls_line("..").is_none());
    }

    #[test]
    fn sort_entries_puts_dirs_first_then_case_insensitive_names() {
        let mut v = vec![
            entry("Zeta", false),
            entry("alpha", false),
            entry("beta", true),
            entry("Archive", true),
        ];
        sort_entries(&mut v);
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Archive", "beta", "alpha", "Zeta"]);
    }

    fn entry(name: &str, is_dir: bool) -> ExplorerEntry {
        ExplorerEntry {
            name: name.into(),
            is_dir,
            size: 0,
            modified: String::new(),
            permissions: String::new(),
        }
    }

    #[test]
    fn list_host_dir_reads_a_real_directory() {
        let dir =
            std::env::temp_dir().join(format!("tvdocker-explore-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
        std::fs::write(dir.join("a.txt"), b"hi").expect("write");
        let rows = list_host_dir(dir.to_str().expect("utf8")).expect("list");
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"sub") && names.contains(&"a.txt"),
            "{names:?}"
        );
        let sub = rows.iter().find(|r| r.name == "sub").expect("sub row");
        assert!(sub.is_dir);
        let file = rows.iter().find(|r| r.name == "a.txt").expect("file row");
        assert_eq!(file.size, 2);
        assert!(!file.permissions.is_empty(), "mode formatted");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_host_dir_on_a_missing_path_errors() {
        assert!(
            list_host_dir("/tvdocker-definitely-missing-dir").is_err(),
            "a missing dir must surface an error, not an empty listing"
        );
    }

    #[test]
    fn volume_container_path_maps_root_and_nested_paths() {
        assert_eq!(volume_container_path("/"), "/volume_data");
        assert_eq!(volume_container_path("/var/log"), "/volume_data/var/log");
        assert_eq!(volume_container_path("/var/log/"), "/volume_data/var/log");
    }

    /// Not run in CI: needs a live daemon AND creates a helper
    /// container. Run with:
    /// `./dcargo test -- --ignored volume_helper_against_real_docker`
    #[tokio::test]
    #[ignore = "needs a Docker daemon"]
    async fn volume_helper_against_real_docker() {
        let docker = super::super::client::connect().expect("daemon reachable");
        // RUN-DISCOVERED (4B final review): a `name:/mount` bind makes
        // Docker AUTO-CREATE a missing named volume, so a helper for an
        // unknown name does NOT fail — it springs into existence. Pin that
        // truth (the Volume Explorer only ever opens volumes from the list,
        // so the auto-create path is unreachable in the app) and clean up.
        let bogus = format!("tvdocker-explore-probe-{}", std::process::id());
        let id = ensure_volume_helper(&docker, &bogus)
            .await
            .expect("Docker auto-creates a missing named volume");
        assert!(!id.is_empty(), "the helper runs");
        remove_volume_helper(&docker, &bogus).await.ok();
        drop_volume_when_released(&docker, &bogus).await;

        // The path mapper is wired through the real helper for an
        // EXISTING volume — read-only listing of its root.
        let existing = docker
            .list_volumes::<String>(None)
            .await
            .expect("list volumes")
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.name)
            .next()
            .expect("this daemon has at least one volume");
        let rows = list_volume_dir(&docker, &existing, "/")
            .await
            .expect("list");
        assert!(rows.iter().all(|r| !r.name.is_empty()));
        remove_volume_helper(&docker, &existing).await.ok();
    }

    /// Remove `volume` once Docker's async auto-remove has released it
    /// (an immediate remove can still see it "in use"): retry a forced
    /// removal for a few seconds, best-effort — a probe must not leave
    /// junk on the daemon.
    async fn drop_volume_when_released(docker: &Docker, volume: &str) {
        for _ in 0..30 {
            if docker
                .remove_volume(
                    volume,
                    Some(bollard::volume::RemoveVolumeOptions { force: true }),
                )
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Not run in CI: needs a live daemon. Run with:
    /// `./dcargo test -- --ignored container_dir_against_real_docker`
    #[tokio::test]
    #[ignore = "needs a Docker daemon"]
    async fn container_dir_against_real_docker() {
        let docker = super::super::client::connect().expect("daemon reachable");
        let rows = list_container_dir(&docker, "tvdocker-no-such-container", "/").await;
        assert!(rows.is_err(), "a bogus container must error, not list");
    }

    #[test]
    fn copy_to_container_tars_the_given_paths() {
        // The tar half is pure over the fs — pin it without a daemon.
        let dir = std::env::temp_dir().join(format!("tvdocker-tar-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::write(dir.join("src/one.txt"), b"one").expect("write");
        std::fs::write(dir.join("two.txt"), b"two-two").expect("write");
        let paths = vec![
            dir.join("src").to_str().expect("utf8").to_string(),
            dir.join("two.txt").to_str().expect("utf8").to_string(),
        ];
        let bytes = tar_bytes(&paths).expect("tar");
        // Two entries (src/ and two.txt) plus the dir's own child.
        let mut archive = tar::Archive::new(bytes.as_slice());
        let names: Vec<String> = archive
            .entries()
            .expect("entries")
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.display().to_string()))
            .collect();
        assert!(
            names.iter().any(|n| n == "src/"),
            "dir entry (tar names dirs with a trailing slash): {names:?}"
        );
        assert!(
            names.iter().any(|n| n.ends_with("one.txt")),
            "child: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "two.txt"),
            "file entry: {names:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copy_to_container_rejects_missing_paths() {
        assert!(
            tar_bytes(&["/tvdocker-definitely-missing".to_string()]).is_err(),
            "a missing source must error before any upload"
        );
    }

    /// Not run in CI: needs a live daemon; writes into a container.
    /// Run with: `./dcargo test -- --ignored copy_roundtrip_against_real_docker`
    #[tokio::test]
    #[ignore = "needs a Docker daemon"]
    async fn copy_roundtrip_against_real_docker() {
        let docker = super::super::client::connect().expect("daemon reachable");
        // A bogus container must error in both directions.
        assert!(
            copy_to_container(
                &docker,
                "tvdocker-no-such-container",
                &["/tmp".into()],
                "/tmp"
            )
            .await
            .is_err()
        );
        assert!(
            copy_from_container(
                &docker,
                "tvdocker-no-such-container",
                &["/etc/hostname".into()],
                "/tmp"
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn host_mutations_round_trip_on_a_temp_dir() {
        let dir = std::env::temp_dir().join(format!("tvdocker-mut-test-{}", std::process::id()));
        let sub = dir.join("work");
        std::fs::create_dir_all(&sub).expect("mkdir");
        let file = sub.join("a.txt");
        std::fs::write(&file, b"x").expect("write");

        create_host_path(sub.join("newdir").to_str().expect("utf8"), true).expect("mkdir");
        assert!(sub.join("newdir").is_dir());
        create_host_path(sub.join("b.txt").to_str().expect("utf8"), false).expect("touch");
        assert!(sub.join("b.txt").is_file());

        rename_host_path(
            sub.join("b.txt").to_str().expect("utf8"),
            sub.join("c.txt").to_str().expect("utf8"),
        )
        .expect("rename");
        assert!(sub.join("c.txt").is_file());

        delete_host_paths(&[
            file.to_str().expect("utf8").to_string(),
            sub.join("newdir").to_str().expect("utf8").to_string(),
            sub.join("c.txt").to_str().expect("utf8").to_string(),
        ])
        .expect("delete");
        assert!(!sub.join("a.txt").exists() && !sub.join("newdir").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_delete_on_a_missing_path_errors() {
        assert!(delete_host_paths(&["/tvdocker-missing-file".into()]).is_err());
    }

    /// Not run in CI: needs a live daemon. Run with:
    /// `./dcargo test -- --ignored mutations_against_real_docker`
    #[tokio::test]
    #[ignore = "needs a Docker daemon"]
    async fn mutations_against_real_docker() {
        let docker = super::super::client::connect().expect("daemon reachable");
        // A bogus container must error for every mutation.
        assert!(
            delete_in_container(&docker, "tvdocker-no-such-container", &["/tmp/x".into()])
                .await
                .is_err()
        );
        assert!(
            create_in_container(&docker, "tvdocker-no-such-container", "/tmp/x", true)
                .await
                .is_err()
        );
        assert!(
            rename_in_container(&docker, "tvdocker-no-such-container", "/tmp/a", "/tmp/b")
                .await
                .is_err()
        );
    }
    /// A helper name must be stable (reopening reuses it) and legal: Docker
    /// only accepts `[a-zA-Z0-9][a-zA-Z0-9_.-]*`, and an image id carries
    /// `sha256:` with a colon.
    #[test]
    fn image_container_name_is_deterministic_and_docker_legal() {
        let id = "sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let a = image_container_name(id);
        assert_eq!(a, image_container_name(id), "deterministic");
        assert!(
            !a.contains(':'),
            "the colon must go (Docker's name regex): {a}"
        );
        assert!(
            a.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()),
            "names must start alphanumeric: {a}"
        );
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')),
            "only the name-regex characters: {a}"
        );
    }

    /// The helper must run the image ITSELF and nothing else: an empty
    /// `entrypoint` override is load-bearing, because Docker PREPENDS the
    /// image's Entrypoint to the Cmd we pass — without it an nginx image
    /// would start `/docker-entrypoint.sh sleep 86400`.
    #[test]
    fn image_helper_config_clears_the_entrypoint() {
        let cfg = image_helper_config("sha256:abc");
        assert_eq!(cfg.image.as_deref(), Some("sha256:abc"), "runs the image");
        assert_eq!(
            cfg.cmd.as_ref().map(|c| c.join(" ")).as_deref(),
            Some("sleep 86400"),
            "sleeps"
        );
        assert_eq!(
            cfg.entrypoint,
            Some(vec![]),
            "the image entrypoint is cleared"
        );
        let hc = cfg.host_config.as_ref().expect("host config");
        assert_eq!(hc.auto_remove, Some(true), "reaped on stop");
    }

    /// Live: creates, reuses, removes. Skipped without a reachable daemon.
    #[tokio::test]
    #[ignore = "needs a Docker daemon and a local image"]
    async fn image_helper_creates_reuses_and_removes() {
        let docker = crate::domain::docker::client::connect().expect("daemon");
        let image = std::env::var("TVDOCKER_TEST_IMAGE").unwrap_or_else(|_| "alpine:latest".into());
        let first = ensure_image_helper(&docker, &image).await.expect("create");
        let second = ensure_image_helper(&docker, &image).await.expect("reuse");
        assert_eq!(first, second, "one helper per image");
        remove_image_helper(&docker, &image).await.expect("remove");
    }
}

/// Not run in CI: needs a live daemon AND writes into a container.
/// Run with:
/// `./dcargo test -- --ignored copy_roundtrip_live_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn copy_roundtrip_live_against_real_docker() {
    use bollard::container::{
        CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    };
    let docker = crate::domain::docker::client::connect().expect("daemon reachable");
    let name = format!("tvdocker-copy-probe-{}", std::process::id());
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: name.as_str(),
                platform: None,
            }),
            bollard::container::Config::<String> {
                image: Some("alpine:latest".into()),
                cmd: Some(vec!["sleep".into(), "86400".into()]),
                ..Default::default()
            },
        )
        .await
        .expect("create");
    docker
        .start_container(&container.id, None::<StartContainerOptions<String>>)
        .await
        .expect("start");
    // host → container: a temp file lands in /tmp of the container.
    let tmp = std::env::temp_dir().join(format!("tvdocker-copy-{}", std::process::id()));
    std::fs::write(&tmp, b"plan4b").expect("write");
    let src = tmp.to_str().expect("utf8").to_string();
    copy_to_container(&docker, &container.id, &[src], "/tmp")
        .await
        .expect("upload");
    let rows = list_container_dir(&docker, &container.id, "/tmp")
        .await
        .expect("list");
    assert!(
        rows.iter()
            .any(|r| { r.name == tmp.file_name().unwrap().to_string_lossy() })
    );
    // container → host: it comes back.
    let back = std::env::temp_dir().join(format!("tvdocker-copy-back-{}", std::process::id()));
    std::fs::create_dir_all(&back).expect("mkdir");
    let entry = format!("/tmp/{}", tmp.file_name().unwrap().to_string_lossy());
    copy_from_container(
        &docker,
        &container.id,
        &[entry],
        back.to_str().expect("utf8"),
    )
    .await
    .expect("download");
    let restored = back.join(tmp.file_name().unwrap());
    assert_eq!(std::fs::read(&restored).expect("read"), b"plan4b");
    docker
        .remove_container(
            &name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
        .expect("cleanup");
    std::fs::remove_file(&tmp).ok();
    std::fs::remove_dir_all(&back).ok();
}
