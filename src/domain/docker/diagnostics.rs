//! The AI-diagnostics context:
//! everything a model needs to
//! diagnose one container — identity, state, a log tail and one stats
//! sample. READ-ONLY: inspect + logs + stats, no mutation.
use anyhow::Result;
use bollard::Docker;

use crate::domain::docker::logs::{self, LogLine};
use crate::domain::docker::statistics::{self, StatEntry};

/// The snapshot handed to the prompt builder. Fields mirror
/// tvdocker's context verbatim so the vendored prompt is unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiagnosticContext {
    pub container_id: String,
    pub container_name: String,
    pub state: String,
    pub exit_code: Option<i32>,
    pub recent_logs: Vec<LogLine>,
    pub stats: Option<StatEntry>,
    pub env_vars: Vec<(String, String)>,
    pub command: String,
    pub image: String,
    pub status: String,
}

/// Collect one container's diagnostic context. The log tail reuses
/// `logs::fetch_logs(.., 0)` (whose `since == 0` rule asks Docker for
/// `tail:"100"`); stats reuse `statistics::one_shot`.
pub async fn collect_diagnostics(docker: &Docker, container_id: &str) -> Result<DiagnosticContext> {
    let inspect = docker.inspect_container(container_id, None).await?;

    let container_name = inspect
        .name
        .as_deref()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string();

    let state = inspect
        .state
        .as_ref()
        .and_then(|s| s.status.as_ref())
        .map(|s| format!("{s:?}"))
        .unwrap_or_default();

    let exit_code = inspect
        .state
        .as_ref()
        .and_then(|s| s.exit_code)
        .map(|c| c as i32);

    let image = inspect
        .config
        .as_ref()
        .and_then(|c| c.image.clone())
        .unwrap_or_default();

    let command = inspect
        .config
        .as_ref()
        .and_then(|c| c.cmd.clone())
        .unwrap_or_default()
        .join(" ");

    let status = inspect
        .state
        .as_ref()
        .and_then(|s| s.error.clone())
        .unwrap_or_else(|| state.clone());

    let env_vars: Vec<(String, String)> = inspect
        .config
        .as_ref()
        .and_then(|c| c.env.clone())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            let (k, v) = e.split_once('=')?;
            if k.is_empty() {
                None
            } else {
                Some((k.to_string(), v.to_string()))
            }
        })
        .collect();

    let recent_logs: Vec<LogLine> = logs::fetch_logs(docker, container_id, 0)
        .await?
        .into_iter()
        .map(|(_, line)| line)
        .collect();

    let stats = if state.eq_ignore_ascii_case("running") {
        statistics::one_shot(docker, container_id, &container_name).await?
    } else {
        None
    };

    Ok(DiagnosticContext {
        container_id: container_id.to_string(),
        container_name,
        state,
        exit_code,
        recent_logs,
        stats,
        env_vars,
        command,
        image,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bogus id must surface as an error (the worker turns it into a
    /// window error phase). Works with OR without a daemon: a missing
    /// socket and an unknown id both fail — failure is the observable.
    #[tokio::test]
    async fn a_bogus_id_errors() {
        let Ok(docker) = crate::domain::docker::client::connect() else {
            return; // no daemon: nothing to assert beyond "connect failed"
        };
        assert!(
            collect_diagnostics(&docker, "tvdocker-no-such-container")
                .await
                .is_err(),
            "an unknown id must not yield a context"
        );
    }

    /// Live, read-only: needs a Docker daemon AND at least one
    /// container. Run with:
    ///   ./dcargo test --locked -- --ignored diagnostics_probe_live
    #[tokio::test]
    #[ignore = "needs a Docker daemon with a container"]
    async fn diagnostics_probe_live() {
        let docker = crate::domain::docker::client::connect().expect("daemon");
        let list = docker
            .list_containers(Some(bollard::container::ListContainersOptions::<String> {
                all: true,
                ..Default::default()
            }))
            .await
            .expect("list");
        let Some(id) = list.first().and_then(|c| c.id.clone()) else {
            return; // no containers: nothing to sample
        };
        let ctx = collect_diagnostics(&docker, &id).await.expect("collect");
        assert_eq!(ctx.container_id, id);
        assert!(ctx.recent_logs.len() <= 100, "tail is bounded");
    }
}
