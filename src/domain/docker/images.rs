//! Image listing (copied from oh-my-docker's `docker/images.rs`,
//! domain code is vendored, then adapted).

use anyhow::Result;
use bollard::Docker;
use bollard::container::{Config, CreateContainerOptions};
use bollard::image::ListImagesOptions;
use bollard::models::ImageSummary;
use bollard::models::{HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use std::collections::HashMap;

/// One row of the Images window (field-for-field the oh-my-docker type).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageEntry {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size: i64,
}

/// Map one bollard row into our entry (pure — unit-tested without
/// Docker). The split follows the vendored source verbatim:
/// `rsplitn(2, ':')` → `parts[0]` is the tag, `parts[1]` the repository,
/// `"latest"` when the reference carries no colon.
pub fn summarize(i: ImageSummary) -> ImageEntry {
    let (repository, tag) = match i.repo_tags.first() {
        Some(rt) => {
            let parts: Vec<&str> = rt.rsplitn(2, ':').collect();
            if parts.len() > 1 {
                (parts[1].to_string(), parts[0].to_string())
            } else {
                (parts[0].to_string(), "latest".to_string())
            }
        }
        None => ("<none>".to_string(), "<none>".to_string()),
    };
    ImageEntry {
        id: i.id,
        repository,
        tag,
        size: i.size,
    }
}

/// List the tagged images, mapped for the UI (`all: false` — vendored).
pub async fn list_images(docker: &Docker) -> Result<Vec<ImageEntry>> {
    let options = ListImagesOptions::<String> {
        all: false,
        ..Default::default()
    };
    let images = docker.list_images(Some(options)).await?;
    Ok(images.into_iter().map(summarize).collect())
}

/// True when the image carries the `<none>:<none>` repo tag (vendored
/// tvdocker `remove_dangling_images` filter — pure, unit-tested).
pub fn is_dangling(repo_tags: &[String]) -> bool {
    repo_tags.iter().any(|t| t.starts_with("<none>:"))
}

/// Remove one image by id (vendored tvdocker `remove_image`).
pub async fn remove_image(docker: &Docker, id: &str) -> Result<()> {
    docker.remove_image(id, None, None).await?;
    Ok(())
}

/// Remove every dangling image; returns how many were REMOVED (vendored
/// tvdocker — the per-image errors are swallowed there too: a single
/// in-use dangling layer must not abort the sweep).
pub async fn remove_dangling_images(docker: &Docker) -> Result<usize> {
    let options = ListImagesOptions::<String> {
        all: true,
        ..Default::default()
    };
    let images = docker.list_images(Some(options)).await?;
    let dangling: Vec<String> = images
        .iter()
        .filter(|i| is_dangling(&i.repo_tags))
        .map(|i| i.id.clone())
        .collect();
    let count = dangling.len();
    for id in &dangling {
        let _ = docker.remove_image(id, None, None).await;
    }
    Ok(count)
}

/// Prune unused images; `(count deleted, bytes reclaimed)` (vendored
/// tvdocker `prune_unused_images`).
pub async fn prune_unused_images(docker: &Docker) -> Result<(usize, i64)> {
    let prune = docker.prune_images::<String>(None).await?;
    let count = prune.images_deleted.map(|d| d.len()).unwrap_or(0);
    let space = prune.space_reclaimed.unwrap_or(0);
    Ok((count, space))
}

/// The status-line info line for a prune
/// (bytes formatted like the table SIZE column, `views::resource::format_size`).
pub fn prune_status_line(count: usize, space: i64) -> String {
    format!(
        "Pruned {count} unused images (freed {})",
        crate::views::resource::format_size(space)
    )
}

/// Every field the run form collects (vendored tvdocker
/// `app::event::ContainerOpts`, field-for-field). Empty string =
/// "leave the container default", matching upstream.
#[derive(Clone, Debug, Default)]
pub struct ContainerOpts {
    pub image: String,
    pub cmd: String,
    pub shell: String,
    pub user: String,
    pub workdir: String,
    pub env_vars: String,
    pub port_mapping: String,
    pub volumes: String,
    pub name: String,
    pub autoremove: bool,
    pub restart_policy: String,
    pub memory_limit: String,
    pub cpu_limit: String,
    pub network: String,
    pub labels: String,
    pub privileged: bool,
}

/// Vendored tvdocker `parse_memory`, accepting the Docker-CLI
/// spellings: `512m` / `512mb` / `1g` / `2gb` / `100k` / plain bytes.
/// Two-char suffixes are matched before one-char (else `2gb` would
/// strip the `g` and choke on `2b`).
pub fn parse_memory(s: &str) -> Result<u64> {
    let lower = s.trim().to_lowercase();
    for (suffix, scale) in [
        ("gb", 1_000_000_000.0),
        ("mb", 1_000_000.0),
        ("kb", 1_000.0),
        ("g", 1_000_000_000.0),
        ("m", 1_000_000.0),
        ("k", 1_000.0),
    ] {
        if let Some(num) = lower.strip_suffix(suffix) {
            return num
                .parse::<f64>()
                .map(|v| (v * scale) as u64)
                .map_err(|e| anyhow::anyhow!("{e}"));
        }
    }
    lower.parse::<u64>().map_err(|e| anyhow::anyhow!("{e}"))
}

/// Turn a user-entered volume spec into a bind string Docker accepts
/// (vendored tvdocker). Docker rejects relative host paths (it reads
/// them as volume names), so `.`/`..`/relative segments resolve against
/// the cwd and `~/` against `$HOME`; named volumes and absolute paths
/// pass through untouched.
pub fn resolve_volume_bind(spec: &str) -> String {
    let Some((host, rest)) = spec.split_once(':') else {
        return spec.to_string();
    };
    let is_named_volume = !host.starts_with('/')
        && !host.starts_with('.')
        && !host.starts_with('~')
        && !host.contains('\\')
        && !(host.len() >= 2 && host.as_bytes()[1] == b':');
    if is_named_volume {
        return spec.to_string();
    }
    let mut path = host.to_string();
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            path = format!("{}/{}", home.trim_end_matches('/'), stripped);
        }
    } else if !path.starts_with('/')
        && let Ok(cwd) = std::env::current_dir()
    {
        path = format!("{}/{}", cwd.display(), path.trim_start_matches("./"));
    }
    format!("{path}:{rest}")
}

/// Create (but do NOT start) a container from the form (vendored
/// tvdocker `create_container`); returns the new container id. Start is
/// the worker's next step (upstream `spawn_create_container` does
/// create-then-start).
pub async fn create_container(docker: &Docker, opts: &ContainerOpts) -> Result<String> {
    let mut config = Config::<String> {
        image: Some(opts.image.clone()),
        tty: Some(true),
        open_stdin: Some(true),
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        ..Default::default()
    };
    if !opts.cmd.is_empty() {
        config.cmd = Some(opts.cmd.split_whitespace().map(String::from).collect());
    } else if !opts.shell.is_empty() {
        config.cmd = Some(vec![opts.shell.clone()]);
    }
    if !opts.user.is_empty() {
        config.user = Some(super::shell::resolve_host_user(&opts.user));
    }
    if !opts.workdir.is_empty() {
        config.working_dir = Some(opts.workdir.clone());
    }
    if !opts.env_vars.is_empty() {
        config.env = Some(
            opts.env_vars
                .split('\n')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        );
    }
    let mut host_config = HostConfig::default();
    if !opts.port_mapping.is_empty() {
        let mut exposed: HashMap<String, HashMap<(), ()>> = HashMap::new();
        let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        for line in opts.port_mapping.split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            exposed.insert(format!("{line} /tcp"), HashMap::new());
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let container_port = parts[parts.len() - 1].trim();
                let host_ports: Vec<&str> = if parts.len() == 2 {
                    vec![parts[0].trim()]
                } else {
                    parts[..parts.len() - 1].iter().map(|s| s.trim()).collect()
                };
                let binds: Vec<PortBinding> = host_ports
                    .iter()
                    .map(|hp| PortBinding {
                        host_ip: None,
                        host_port: Some(hp.to_string()),
                    })
                    .collect();
                bindings.insert(format!("{container_port} /tcp"), Some(binds));
            }
        }
        if !exposed.is_empty() {
            config.exposed_ports = Some(exposed);
        }
        if !bindings.is_empty() {
            host_config.port_bindings = Some(bindings);
        }
    }
    if !opts.volumes.is_empty() {
        let binds: Vec<String> = opts
            .volumes
            .split('\n')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(resolve_volume_bind)
            .collect();
        if !binds.is_empty() {
            host_config.binds = Some(binds);
        }
    }
    if opts.autoremove {
        host_config.auto_remove = Some(true);
    }
    if !opts.restart_policy.is_empty() {
        let policy_name = match opts.restart_policy.as_str() {
            "always" => RestartPolicyNameEnum::ALWAYS,
            "on-failure" => RestartPolicyNameEnum::ON_FAILURE,
            "unless-stopped" => RestartPolicyNameEnum::UNLESS_STOPPED,
            _ => RestartPolicyNameEnum::NO,
        };
        host_config.restart_policy = Some(RestartPolicy {
            name: Some(policy_name),
            ..Default::default()
        });
    }
    if !opts.memory_limit.is_empty()
        && let Ok(bytes) = parse_memory(&opts.memory_limit)
    {
        host_config.memory = Some(bytes as i64);
    }
    if !opts.cpu_limit.is_empty()
        && let Ok(cpu) = opts.cpu_limit.parse::<f64>()
    {
        host_config.nano_cpus = Some((cpu * 1_000_000_000.0) as i64);
    }
    if !opts.network.is_empty() {
        host_config.network_mode = Some(opts.network.clone());
    }
    if opts.privileged {
        host_config.privileged = Some(true);
    }
    if !opts.labels.is_empty() {
        let labels: HashMap<String, String> = opts
            .labels
            .split('\n')
            .filter_map(|line| {
                let mut parts = line.splitn(2, '=');
                match (parts.next(), parts.next()) {
                    (Some(k), Some(v)) => Some((k.to_string(), v.to_string())),
                    _ => None,
                }
            })
            .collect();
        if !labels.is_empty() {
            config.labels = Some(labels);
        }
    }
    config.host_config = Some(host_config);
    let result = if opts.name.is_empty() {
        docker
            .create_container(None::<CreateContainerOptions<&str>>, config)
            .await?
    } else {
        docker
            .create_container(
                Some(CreateContainerOptions {
                    name: opts.name.clone(),
                    platform: None,
                }),
                config,
            )
            .await?
    };
    Ok(result.id)
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored images_create_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn images_create_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    // A bogus image must ERROR at create — no container is left behind.
    let opts = ContainerOpts {
        image: "tvdocker-no-such-image".into(),
        ..Default::default()
    };
    assert!(create_container(&docker, &opts).await.is_err());
}

/// Not run in CI: needs a live daemon AND deletes the container it
/// creates. Run with:
/// `./dcargo test -- --ignored images_run_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn images_run_against_real_docker() {
    use bollard::container::StartContainerOptions;
    let docker = super::client::connect().expect("daemon reachable");
    let opts = ContainerOpts {
        image: "alpine:latest".into(),
        cmd: "true".into(),
        name: "tvdocker-plan4a-probe".into(),
        ..Default::default()
    };
    let id = create_container(&docker, &opts).await.expect("create");
    docker
        .start_container(&id, None::<StartContainerOptions<String>>)
        .await
        .expect("start");
    docker
        .remove_container(&id, None::<bollard::container::RemoveContainerOptions>)
        .await
        .expect("cleanup");
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored images_remove_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn images_remove_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    // A bogus id must ERROR — no host mutation, observable either way.
    assert!(
        remove_image(&docker, "tvdocker-no-such-image")
            .await
            .is_err()
    );
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored images_list_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn images_list_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    let rows = list_images(&docker).await.expect("list succeeds");
    // Shape, not content: every row carries an id and a repo name.
    assert!(
        rows.iter()
            .all(|r| !r.id.is_empty() && !r.repository.is_empty())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(repo_tags: Vec<String>) -> ImageSummary {
        ImageSummary {
            id: "sha256:1111222233334444".into(),
            repo_tags,
            size: 1_500_000,
            ..Default::default()
        }
    }

    #[test]
    fn splits_the_first_repo_tag_into_repository_and_tag() {
        let e = summarize(raw(vec!["nginx:1.27".into()]));
        assert_eq!(e.repository, "nginx");
        assert_eq!(e.tag, "1.27");
        assert_eq!(e.id, "sha256:1111222233334444");
        assert_eq!(e.size, 1_500_000);
    }

    #[test]
    fn a_reference_without_a_colon_gets_the_latest_tag() {
        // `rsplitn(2, ':')` yields one part ⇒ tag defaults — vendored
        // behavior, kept verbatim.
        let e = summarize(raw(vec!["myapp".into()]));
        assert_eq!((e.repository.as_str(), e.tag.as_str()), ("myapp", "latest"));
    }

    #[test]
    fn is_dangling_detects_the_none_repo_tag() {
        assert!(is_dangling(&["<none>:<none>".into()]));
        assert!(is_dangling(&["nginx:1.27".into(), "<none>:<none>".into()]));
        assert!(!is_dangling(&["nginx:1.27".into()]));
        assert!(!is_dangling(&[]));
    }

    #[test]
    fn prune_status_line_names_count_and_freed_bytes() {
        assert_eq!(
            prune_status_line(3, 1_200_000_000),
            "Pruned 3 unused images (freed 1.2GB)"
        );
        assert_eq!(prune_status_line(0, 0), "Pruned 0 unused images (freed 0B)");
    }

    #[test]
    fn parse_memory_honours_g_m_k_suffixes() {
        assert_eq!(parse_memory("512m").expect("m"), 512_000_000);
        assert_eq!(parse_memory("1g").expect("g"), 1_000_000_000);
        assert_eq!(parse_memory("100k").expect("k"), 100_000);
        assert_eq!(parse_memory("2048").expect("plain"), 2048);
        assert!(parse_memory("lots").is_err(), "garbage must error");
        assert_eq!(parse_memory(" 1G ").expect("trimmed"), 1_000_000_000);
    }

    #[test]
    fn parse_memory_accepts_the_cli_mb_gb_kb_spellings() {
        // Docker CLI accepts `512mb` as happily as `512m`; silently
        // dropping the limit (the old `if let Ok` path) is worse than
        // either spelling.
        assert_eq!(parse_memory("512mb").expect("mb"), 512_000_000);
        assert_eq!(parse_memory("2GB").expect("GB"), 2_000_000_000);
        assert_eq!(parse_memory("100kb").expect("kb"), 100_000);
    }

    #[test]
    fn resolve_volume_bind_passes_named_volumes_through() {
        assert_eq!(
            resolve_volume_bind("data:/var/lib/data"),
            "data:/var/lib/data"
        );
    }

    #[test]
    fn resolve_volume_bind_expands_home_and_resolves_relative_paths() {
        let home = std::env::var("HOME").expect("test env has HOME");
        assert_eq!(
            resolve_volume_bind("~/src:/app"),
            format!("{home}/src:/app")
        );
        let cwd = std::env::current_dir().expect("cwd");
        assert_eq!(
            resolve_volume_bind("./local:/app"),
            format!("{}/local:/app", cwd.display())
        );
        assert_eq!(
            resolve_volume_bind("/abs:/app"),
            "/abs:/app",
            "absolute paths pass through"
        );
    }

    #[test]
    fn untagged_images_fall_back_to_none_slash_none() {
        let e = summarize(ImageSummary::default());
        assert_eq!(e.repository, "<none>");
        assert_eq!(e.tag, "<none>");
        assert_eq!(e.size, 0);
    }
}
