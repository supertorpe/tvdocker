//! The task worker ("actions: …"): one task
//! consumes `TaskReq` over an unbounded mpsc — a single consumer, so
//! receivers ride along un-cloned — and performs every Docker side
//! effect the UI thread cannot (it has no runtime handle). Results
//! return as `AppMsg`; successful mutations re-request a list poll so
//! the UI reflects them immediately instead of on the next tick.

use super::{client, details, logs, shell};
use crate::core::{ActionReq, AppMsg, DiagnosticsPhase, TaskReq};
use crate::domain::config::LlmConfig;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::broadcast::Sender as ActionSender;
use tokio::sync::mpsc::Sender;

/// The logs poll cadence (vendored — tvdocker re-issued every 500 ms).
const LOGS_POLL_MS: u64 = 500;
/// Consecutive failures before the window shows `(reconnecting…)`
/// (one flaky tick must not flicker the title row).
const LOGS_FAILURE_STREAK: u32 = 3;

/// Spawn the single `TaskReq` consumer. Lives for the
/// whole app (`app::run`); `TaskError` is the ONLY failure channel —
/// §10 forbids modals for background errors.
pub fn spawn_task_worker(
    app_tx: Sender<AppMsg>,
    mut tasks: tokio::sync::mpsc::UnboundedReceiver<TaskReq>,
    action_tx: ActionSender<ActionReq>,
) {
    tokio::spawn(async move {
        while let Some(req) = tasks.recv().await {
            match req {
                TaskReq::Inspect { id } => {
                    let outcome: anyhow::Result<crate::domain::docker::details::Details> =
                        match client::connect() {
                            Ok(docker) => details::inspect_container(&docker, &id).await,
                            Err(e) => Err(e),
                        };
                    let what = format!("inspect {id}");
                    report(&app_tx, &what, outcome, |details| AppMsg::Details {
                        id,
                        details,
                    })
                    .await;
                }
                TaskReq::RemoveContainer { id } => {
                    let outcome = remove(&id).await;
                    finish(&app_tx, &action_tx, outcome, &format!("remove {id}")).await;
                }
                TaskReq::SetContainerState { id, start } => {
                    let outcome = set_state(&id, start).await;
                    let what = if start { "start" } else { "stop" };
                    finish(&app_tx, &action_tx, outcome, &format!("{what} {id}")).await;
                }
                TaskReq::RemoveImage { id } => {
                    let outcome = async {
                        let docker = client::connect()?;
                        super::images::remove_image(&docker, &id).await
                    }
                    .await;
                    finish(&app_tx, &action_tx, outcome, &format!("remove image {id}")).await;
                }
                TaskReq::RemoveNetwork { id } => {
                    let outcome = async {
                        let docker = client::connect()?;
                        super::networks::remove_network(&docker, &id).await
                    }
                    .await;
                    finish(
                        &app_tx,
                        &action_tx,
                        outcome,
                        &format!("remove network {id}"),
                    )
                    .await;
                }
                TaskReq::RemoveVolume { name } => {
                    let outcome = async {
                        let docker = client::connect()?;
                        super::volumes::remove_volume(&docker, &name).await
                    }
                    .await;
                    finish(
                        &app_tx,
                        &action_tx,
                        outcome,
                        &format!("remove volume {name}"),
                    )
                    .await;
                }
                TaskReq::RemoveDanglingImages => {
                    let outcome = async {
                        let docker = client::connect()?;
                        super::images::remove_dangling_images(&docker).await
                    }
                    .await;
                    if report(&app_tx, "remove dangling", outcome, |count| {
                        AppMsg::Status(format!("Removed {count} dangling images"))
                    })
                    .await
                    {
                        let _ = action_tx.send(ActionReq::PollNow);
                    }
                }
                TaskReq::PruneImages => {
                    let outcome = async {
                        let docker = client::connect()?;
                        super::images::prune_unused_images(&docker).await
                    }
                    .await;
                    if report(&app_tx, "prune images", outcome, |(count, space)| {
                        AppMsg::Status(super::images::prune_status_line(count, space))
                    })
                    .await
                    {
                        let _ = action_tx.send(ActionReq::PollNow);
                    }
                }
                TaskReq::RunContainer { opts } => {
                    let outcome = async {
                        let docker = client::connect()?;
                        let id = super::images::create_container(&docker, &opts).await?;
                        docker
                            .start_container(
                                &id,
                                None::<bollard::container::StartContainerOptions<String>>,
                            )
                            .await?;
                        Ok::<(), anyhow::Error>(())
                    }
                    .await;
                    finish(
                        &app_tx,
                        &action_tx,
                        outcome,
                        &format!("run container from {}", opts.image),
                    )
                    .await;
                }
                TaskReq::StartLogs { id, stop } => {
                    spawn_logs(app_tx.clone(), id, stop);
                }
                TaskReq::StartDiagnostics { id, run, config } => {
                    spawn_diagnostics(app_tx.clone(), id, run, config);
                }
                TaskReq::StartShell {
                    id,
                    shell,
                    user,
                    workdir,
                    out_tx,
                    input_rx,
                    resize_rx,
                } => {
                    shell::spawn_shell(id, shell, user, workdir, out_tx, input_rx, resize_rx);
                }
                TaskReq::UpdateInstall { version, url } => {
                    spawn_install(app_tx.clone(), version, url);
                }
                TaskReq::ExplorerList { key, target, path } => {
                    let outcome = list_for(&target, &path).await;
                    let what = format!("list {path}");
                    report(&app_tx, &what, outcome, |entries| AppMsg::ExplorerDir {
                        key,
                        target,
                        path,
                        entries,
                    })
                    .await;
                }
                TaskReq::ExplorerCopy {
                    key,
                    target,
                    to_container,
                    items,
                    dst,
                } => {
                    let outcome = copy_for(&target, to_container, &items, &dst).await;
                    let relist = copy_relist_target(&target, to_container);
                    finish_explorer(&app_tx, &action_tx, outcome, &key, &relist, dst, "copy").await;
                }
                TaskReq::ExplorerDelete { key, target, paths } => {
                    let outcome = mutate_for(&target, paths.clone(), true).await;
                    finish_explorer(
                        &app_tx,
                        &action_tx,
                        outcome,
                        &key,
                        &target,
                        parent_of(&paths),
                        "delete",
                    )
                    .await;
                }
                TaskReq::ExplorerCreate {
                    key,
                    target,
                    path,
                    is_dir,
                } => {
                    let outcome = create_for(&target, &path, is_dir).await;
                    finish_explorer(
                        &app_tx,
                        &action_tx,
                        outcome,
                        &key,
                        &target,
                        parent_of(&[path]),
                        "create",
                    )
                    .await;
                }
                TaskReq::ExplorerRename {
                    key,
                    target,
                    old,
                    new,
                } => {
                    let outcome = rename_for(&target, &old, &new).await;
                    finish_explorer(
                        &app_tx,
                        &action_tx,
                        outcome,
                        &key,
                        &target,
                        parent_of(&[old]),
                        "rename",
                    )
                    .await;
                }
                TaskReq::RemoveVolumeHelper { name } => {
                    if let Ok(docker) = client::connect() {
                        let _ = super::explorer::remove_volume_helper(&docker, &name).await;
                    }
                }
                TaskReq::RemoveImageHelper { image } => {
                    if let Ok(docker) = client::connect() {
                        let _ = super::explorer::remove_image_helper(&docker, &image).await;
                    }
                }
                TaskReq::AnalyzeImageLayers { key, image } => {
                    // Progress rides the bounded app channel fire-and-forget:
                    // a full channel drops the tick (the next one supersedes
                    // it) — [`poller::offer`]'s documented contract.
                    let ptx = app_tx.clone();
                    let pkey = key.clone();
                    let outcome = super::layers::fetch(&image, move |bytes| {
                        let _ = super::poller::offer(
                            &ptx,
                            AppMsg::LayerProgress {
                                key: pkey.clone(),
                                bytes,
                            },
                        );
                    })
                    .await;
                    match outcome {
                        Ok(analysis) => {
                            let _ = app_tx
                                .send(AppMsg::ImageLayers {
                                    key,
                                    analysis: Box::new(analysis),
                                })
                                .await;
                        }
                        Err(err) => {
                            let _ = app_tx
                                .send(AppMsg::TaskError(format!("layers {image}: {err}")))
                                .await;
                        }
                    }
                }
            }
        }
    });
}

/// Remove one container (the desktop sends one request per target).
async fn remove(id: &str) -> anyhow::Result<()> {
    let docker = client::connect()?;
    docker
        .remove_container(id, None::<bollard::container::RemoveContainerOptions>)
        .await?;
    Ok(())
}

/// Start/stop one container (bollard errors on no-op transitions, so
/// the DESKTOP filters targets by state before sending).
async fn set_state(id: &str, start: bool) -> anyhow::Result<()> {
    let docker = client::connect()?;
    if start {
        docker
            .start_container(
                id,
                None::<bollard::container::StartContainerOptions<String>>,
            )
            .await?;
    } else {
        docker
            .stop_container(id, None::<bollard::container::StopContainerOptions>)
            .await?;
    }
    Ok(())
}

/// Mutation epilogue: success ⇒ ask every poller for a fresh list;
/// failure ⇒ `TaskError`.
/// Publish a success payload, or turn the error into
/// `AppMsg::TaskError("<what>: <cause>")` — the half of [`finish`] that the
/// payload-bearing arms repeated four times with the same shape. Returns
/// whether it succeeded, so a caller that also re-polls only does so on
/// success (an error must not ask for a refresh).
async fn report<T>(
    app_tx: &Sender<AppMsg>,
    what: &str,
    outcome: anyhow::Result<T>,
    ok: impl FnOnce(T) -> AppMsg,
) -> bool {
    match outcome {
        Ok(value) => {
            let _ = app_tx.send(ok(value)).await;
            true
        }
        Err(e) => {
            let _ = app_tx.send(AppMsg::TaskError(format!("{what}: {e}"))).await;
            false
        }
    }
}

async fn finish(
    app_tx: &Sender<AppMsg>,
    action_tx: &ActionSender<ActionReq>,
    outcome: anyhow::Result<()>,
    what: &str,
) {
    match outcome {
        Ok(()) => {
            let _ = action_tx.send(ActionReq::PollNow);
        }
        Err(e) => {
            let _ = app_tx.send(AppMsg::TaskError(format!("{what}: {e}"))).await;
        }
    }
}

/// The parent dir of a path (the mutation's re-list target).
fn parent_of(paths: &[String]) -> String {
    paths
        .first()
        .and_then(|p| std::path::Path::new(p).parent())
        .and_then(|p| p.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "/".to_string())
}

async fn list_for(
    target: &crate::core::ExplorerTarget,
    path: &str,
) -> anyhow::Result<Vec<super::explorer::ExplorerEntry>> {
    match target {
        crate::core::ExplorerTarget::Host => {
            let p = path.to_string();
            tokio::task::spawn_blocking(move || super::explorer::list_host_dir(&p)).await?
        }
        crate::core::ExplorerTarget::Container(id) => {
            let docker = client::connect()?;
            super::explorer::list_container_dir(&docker, id, path).await
        }
        crate::core::ExplorerTarget::Image(image_id) => {
            let docker = client::connect()?;
            super::explorer::list_image_dir(&docker, image_id, path).await
        }
        crate::core::ExplorerTarget::Volume(name) => {
            let docker = client::connect()?;
            super::explorer::list_volume_dir(&docker, name, path).await
        }
    }
}

/// Resolve the docker id a remote mutation runs against (volume
/// windows go through the helper).
async fn remote_id(target: &crate::core::ExplorerTarget) -> anyhow::Result<Option<String>> {
    match target {
        crate::core::ExplorerTarget::Host => Ok(None),
        crate::core::ExplorerTarget::Container(id) => Ok(Some(id.clone())),
        crate::core::ExplorerTarget::Image(image_id) => {
            let docker = client::connect()?;
            Ok(Some(
                super::explorer::ensure_image_helper(&docker, image_id).await?,
            ))
        }
        crate::core::ExplorerTarget::Volume(name) => {
            let docker = client::connect()?;
            Ok(Some(
                super::explorer::ensure_volume_helper(&docker, name).await?,
            ))
        }
    }
}

async fn copy_for(
    target: &crate::core::ExplorerTarget,
    to_container: bool,
    items: &[String],
    dst: &str,
) -> anyhow::Result<()> {
    // …and copy is the ONE allowed image operation, but only outward
    // (image → host); writing into the image is refused .
    if matches!(target, crate::core::ExplorerTarget::Image(_)) && to_container {
        anyhow::bail!("the image pane only allows copying TO the host");
    }
    let docker = client::connect()?;
    let Some(id) = remote_id(target).await? else {
        return Err(anyhow::anyhow!("copy needs a remote target"));
    };
    if to_container {
        // `items` are HOST paths (unmapped); the destination is in-container.
        let dst = remote_path(target, dst);
        super::explorer::copy_to_container(&docker, &id, items, &dst).await?;
    } else {
        // `items` are the REMOTE panel's paths; `dst` is a host dir.
        let items_owned = remote_paths(target, items);
        let dst_owned = dst.to_string();
        super::explorer::copy_from_container(&docker, &id, &items_owned, &dst_owned).await?;
    }
    Ok(())
}

/// The in-container form of one REMOTE-side path.
/// A volume's browsed path is LOGICAL (`/sub/f`) while its helper
/// mounts the volume at `/volume_data`, so the path must be mapped before
/// any in-container use — listing already maps it in `list_volume_dir`;
/// copy-from, delete, create and rename must map too (they did not, so
/// those operations silently hit paths that do not exist in the helper).
/// Host and Container targets use the path unchanged.
fn remote_path(target: &crate::core::ExplorerTarget, path: &str) -> String {
    match target {
        crate::core::ExplorerTarget::Volume(_) => super::explorer::volume_container_path(path),
        _ => path.to_string(),
    }
}

/// [`remote_path`] over a batch (copy sources, delete targets).
fn remote_paths(target: &crate::core::ExplorerTarget, paths: &[String]) -> Vec<String> {
    paths.iter().map(|p| remote_path(target, p)).collect()
}

async fn mutate_for(
    target: &crate::core::ExplorerTarget,
    paths: Vec<String>,
    delete: bool,
) -> anyhow::Result<()> {
    // The image pane is read-only (image-explorer): the view refuses,
    // and the worker refuses again — the guarantee must not depend on the UI.
    if matches!(target, crate::core::ExplorerTarget::Image(_)) {
        anyhow::bail!("the image pane is read-only");
    }

    if delete {
        match target {
            crate::core::ExplorerTarget::Host => {
                tokio::task::spawn_blocking(move || super::explorer::delete_host_paths(&paths))
                    .await??;
            }
            _ => {
                let docker = client::connect()?;
                let Some(id) = remote_id(target).await? else {
                    return Err(anyhow::anyhow!("host mutation handled above"));
                };
                let paths = remote_paths(target, &paths);
                super::explorer::delete_in_container(&docker, &id, &paths).await?;
            }
        }
    }
    Ok(())
}

async fn create_for(
    target: &crate::core::ExplorerTarget,
    path: &str,
    is_dir: bool,
) -> anyhow::Result<()> {
    // The image pane is read-only (image-explorer): the view refuses,
    // and the worker refuses again — the guarantee must not depend on the UI.
    if matches!(target, crate::core::ExplorerTarget::Image(_)) {
        anyhow::bail!("the image pane is read-only");
    }

    match target {
        crate::core::ExplorerTarget::Host => {
            let p = path.to_string();
            tokio::task::spawn_blocking(move || super::explorer::create_host_path(&p, is_dir))
                .await??;
        }
        _ => {
            let docker = client::connect()?;
            let Some(id) = remote_id(target).await? else {
                return Err(anyhow::anyhow!("host mutation handled above"));
            };
            let path = remote_path(target, path);
            super::explorer::create_in_container(&docker, &id, &path, is_dir).await?;
        }
    }
    Ok(())
}

async fn rename_for(
    target: &crate::core::ExplorerTarget,
    old: &str,
    new: &str,
) -> anyhow::Result<()> {
    // The image pane is read-only (image-explorer): the view refuses,
    // and the worker refuses again — the guarantee must not depend on the UI.
    if matches!(target, crate::core::ExplorerTarget::Image(_)) {
        anyhow::bail!("the image pane is read-only");
    }

    match target {
        crate::core::ExplorerTarget::Host => {
            let o = old.to_string();
            let n = new.to_string();
            tokio::task::spawn_blocking(move || super::explorer::rename_host_path(&o, &n))
                .await??;
        }
        _ => {
            let docker = client::connect()?;
            let Some(id) = remote_id(target).await? else {
                return Err(anyhow::anyhow!("host mutation handled above"));
            };
            let old = remote_path(target, old);
            let new = remote_path(target, new);
            super::explorer::rename_in_container(&docker, &id, &old, &new).await?;
        }
    }
    Ok(())
}

/// The side a successful copy must re-list — its DESTINATION. Copying
/// `to_container` refreshes the remote panel (the window's `target`);
/// copying to the host refreshes the HOST panel, so the epilogue must
/// not reuse the remote `target` for a host `dst` (it would list the
/// host path inside the container and key the result to the wrong side).
fn copy_relist_target(
    target: &crate::core::ExplorerTarget,
    to_container: bool,
) -> crate::core::ExplorerTarget {
    if to_container {
        target.clone()
    } else {
        crate::core::ExplorerTarget::Host
    }
}

/// Mutation epilogue (ruling 4): success ⇒ Status + PollNow + the
/// affected dir re-listed (the window never waits for a round trip);
/// failure ⇒ TaskError. `relist_target` is the SIDE the operation
/// touched — not necessarily the window's remote target.
async fn finish_explorer(
    app_tx: &Sender<AppMsg>,
    action_tx: &ActionSender<ActionReq>,
    outcome: anyhow::Result<()>,
    key: &str,
    relist_target: &crate::core::ExplorerTarget,
    relist_path: String,
    what: &str,
) {
    match outcome {
        Ok(()) => {
            let _ = app_tx.send(AppMsg::Status(format!("{what} ok"))).await;
            let _ = action_tx.send(ActionReq::PollNow);
            if let Ok(entries) = list_for(relist_target, &relist_path).await {
                let _ = app_tx
                    .send(AppMsg::ExplorerDir {
                        key: key.to_string(),
                        target: relist_target.clone(),
                        path: relist_path,
                        entries,
                    })
                    .await;
            }
        }
        Err(e) => {
            let _ = app_tx.send(AppMsg::TaskError(format!("{what}: {e}"))).await;
        }
    }
}

/// One container's fresh-tail log session: a poll every
/// 500 ms with `since = latest + 1` NANOS (the first call tails the
/// last 100 lines), batches published as `LogLines`; health flips to
/// `(reconnecting…)` after `LOGS_FAILURE_STREAK` failures and back on
/// recovery (each flip announced exactly once). Exits when the UI-side
/// `stop` flag flips — set by `LogsWindow::drop`.
fn spawn_logs(app_tx: Sender<AppMsg>, id: String, stop: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let mut latest: i64 = 0;
        let mut failures: u32 = 0;
        // The core buffer starts healthy (desktop-created session), so
        // the first announcement must be the FAILURE flip.
        let mut announced_ok = true;
        let mut docker: Option<bollard::Docker> = None;
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            // (Re)connect lazily; a missing socket counts as a failure.
            let conn = match &docker {
                Some(d) => Some(d.clone()),
                None => match client::connect() {
                    Ok(d) => {
                        docker = Some(d.clone());
                        Some(d)
                    }
                    Err(_) => None,
                },
            };
            match conn {
                Some(conn) => {
                    // `latest` is NANOS; Docker's `since` is SECONDS
                    // (see `logs::since_seconds`). The floor re-covers the
                    // boundary second, which `take_new` then dedupes.
                    let since = logs::since_seconds(latest);
                    match logs::fetch_logs(&conn, &id, since).await {
                        Ok(lines) => {
                            failures = 0;
                            if !std::mem::replace(&mut announced_ok, true) {
                                let _ = app_tx
                                    .send(AppMsg::LogStatus {
                                        id: id.clone(),
                                        ok: true,
                                    })
                                    .await;
                            }
                            let (batch, new_mark) = logs::take_new(lines, latest);
                            latest = new_mark;
                            if !batch.is_empty() {
                                let _ = app_tx
                                    .send(AppMsg::LogLines {
                                        id: id.clone(),
                                        lines: batch,
                                    })
                                    .await;
                            }
                        }
                        Err(_) => {
                            note_logs_failure(&app_tx, &id, &mut failures, &mut announced_ok).await;
                        }
                    }
                }
                None => {
                    note_logs_failure(&app_tx, &id, &mut failures, &mut announced_ok).await;
                }
            }
            // The sleep IS the delay behavior: 500 ms between passes, a
            // slow fetch never burst-compensated; first pass immediate.
            tokio::time::sleep(std::time::Duration::from_millis(LOGS_POLL_MS)).await;
        }
    });
}

/// One detached diagnostics run: collect the context, call
/// the LLM, stream the phases as `AppMsg`s. Detached (`spawn_logs`
/// precedent) so a slow provider never blocks the single task loop.
/// Every message echoes `run`, so `AppCore::apply` can drop a stale run.
fn spawn_diagnostics(app_tx: Sender<AppMsg>, id: String, run: u64, config: LlmConfig) {
    tokio::spawn(async move {
        // Collect (read-only): inspect + tail-100 logs + one sample.
        let outcome = async {
            let docker = client::connect()?;
            crate::domain::docker::diagnostics::collect_diagnostics(&docker, &id).await
        }
        .await;
        let ctx = match outcome {
            Ok(ctx) => ctx,
            Err(e) => {
                let message = format!("collect {id}: {e}");
                let _ = app_tx
                    .send(AppMsg::DiagnosticsError { id, run, message })
                    .await;
                return;
            }
        };
        let _ = app_tx
            .send(AppMsg::DiagnosticsPhase {
                id: id.clone(),
                run,
                phase: DiagnosticsPhase::Analyzing,
            })
            .await;
        match crate::domain::llm::request_diagnostics(&config, &ctx).await {
            Ok((analysis, playbook)) => {
                let _ = app_tx
                    .send(AppMsg::DiagnosticsAnalysis {
                        id: id.clone(),
                        run,
                        text: analysis,
                    })
                    .await;
                let _ = app_tx
                    .send(AppMsg::DiagnosticsPlaybook {
                        id: id.clone(),
                        run,
                        text: playbook,
                    })
                    .await;
                let _ = app_tx.send(AppMsg::DiagnosticsDone { id, run }).await;
            }
            Err(e) => {
                let message = format!("diagnose {id}: {e}");
                let _ = app_tx
                    .send(AppMsg::DiagnosticsError { id, run, message })
                    .await;
            }
        }
    });
}

/// Download + swap the running binary, reporting through the status
/// latch — the download/extract/swap runs on the blocking pool so the
/// UI never freezes mid-install.
fn spawn_install(app_tx: Sender<AppMsg>, version: String, url: String) {
    let announced = version.clone();
    tokio::spawn(async move {
        let joined = tokio::task::spawn_blocking(move || {
            crate::domain::update::perform_update(&version, &url)
        })
        .await;
        let outcome = match joined {
            Ok(result) => result,
            Err(join) => Err(anyhow::anyhow!("update task: {join}")),
        };
        let msg = match outcome {
            Ok(()) => AppMsg::Status(crate::domain::update::install_done_text(&announced)),
            Err(e) => AppMsg::TaskError(crate::domain::update::install_failed_text(&e)),
        };
        let _ = app_tx.send(msg).await;
    });
}

/// Count one failure; announce the reconnect state exactly once when
/// the streak threshold is crossed.
async fn note_logs_failure(
    app_tx: &Sender<AppMsg>,
    id: &str,
    failures: &mut u32,
    announced_ok: &mut bool,
) {
    *failures = failures.saturating_add(1);
    if *failures >= LOGS_FAILURE_STREAK && std::mem::replace(announced_ok, false) {
        let _ = app_tx
            .send(AppMsg::LogStatus {
                id: id.to_string(),
                ok: false,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::pty::{PtyMsg, PtyPhase};

    /// Alias for worker harness tuple (needed to satisfy clippy::type_complexity).
    type WorkerHarness = (
        Sender<AppMsg>,
        tokio::sync::mpsc::Receiver<AppMsg>,
        ActionSender<ActionReq>,
        tokio::sync::broadcast::Receiver<ActionReq>,
        tokio::sync::mpsc::UnboundedSender<TaskReq>,
    );

    /// Worker harness: real worker, real (bounded) channels, no
    /// windows. The assertions work with OR without a daemon — a
    /// missing socket and a bogus id both fail, and failure is the
    /// observable we pin.
    fn worker() -> WorkerHarness {
        let (app_tx, app_rx) = tokio::sync::mpsc::channel::<AppMsg>(8);
        let (action_tx, action_rx) = tokio::sync::broadcast::channel::<ActionReq>(4);
        let (task_tx, task_rx) = tokio::sync::mpsc::unbounded_channel::<TaskReq>();
        spawn_task_worker(app_tx.clone(), task_rx, action_tx.clone());
        (app_tx, app_rx, action_tx, action_rx, task_tx)
    }

    #[tokio::test]
    async fn inspect_failure_surfaces_as_task_error_never_a_modal() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::Inspect {
                id: "tvdocker-no-such-container".into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("worker answered")
            .expect("channel open");
        assert!(
            matches!(msg, AppMsg::TaskError(_)),
            "§10: background errors are messages, not modals: {msg:?}"
        );
    }

    #[tokio::test]
    async fn a_layer_request_with_no_docker_latches_a_task_error() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::AnalyzeImageLayers {
                key: "sha256:aaa".into(),
                image: "sha256:aaa".into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("worker answered")
            .expect("channel open");
        assert!(
            matches!(msg, AppMsg::TaskError(_)),
            "§10: a background failure is a message, not a modal: {msg:?}"
        );
    }

    #[tokio::test]
    async fn update_install_failure_surfaces_as_a_latched_error_naming_the_url() {
        // The real arm: perform_update against loopback port 9 (discard)
        // — refused instantly, retried, then reported. Host-mutating only
        // if a binary were actually fetched, which never happens here.
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::UpdateInstall {
                version: "9.9.9".into(),
                url: "http://127.0.0.1:9/archive.tar.gz".into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(15), app_rx.recv())
            .await
            .expect("worker answered")
            .expect("channel open");
        match msg {
            AppMsg::TaskError(text) => {
                assert!(text.contains("Update failed"), "{text}");
                assert!(text.contains("127.0.0.1:9"), "the URL: {text}");
            }
            other => panic!("a failed install is a latched error, not {other:?}"),
        }
    }

    #[tokio::test]
    async fn removing_an_unknown_container_errors_without_requesting_a_poll() {
        let (_app_tx, mut app_rx, _action_tx, mut action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::RemoveContainer {
                id: "tvdocker-no-such-container".into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("worker answered")
            .expect("channel open");
        assert!(matches!(msg, AppMsg::TaskError(_)), "failure: {msg:?}");
        assert!(
            action_rx.try_recv().is_err(),
            "PollNow is for SUCCESS only — no pointless refresh on failure"
        );
    }

    #[tokio::test]
    async fn remove_image_bogus_id_errors_without_requesting_a_poll() {
        let (_app_tx, mut app_rx, _action_tx, mut action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::RemoveImage {
                id: "tvdocker-no-such-image".into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("worker answered")
            .expect("channel open");
        assert!(matches!(msg, AppMsg::TaskError(_)), "{msg:?}");
        assert!(
            action_rx.try_recv().is_err(),
            "failure must NOT request a poll"
        );
    }

    #[tokio::test]
    async fn remove_network_and_volume_bogus_ids_error() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::RemoveNetwork {
                id: "tvdocker-no-such-net".into(),
            })
            .expect("alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("answered")
            .expect("open");
        assert!(matches!(msg, AppMsg::TaskError(_)), "{msg:?}");
        task_tx
            .send(TaskReq::RemoveVolume {
                name: "tvdocker-no-such-vol".into(),
            })
            .expect("alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("answered")
            .expect("open");
        assert!(matches!(msg, AppMsg::TaskError(_)), "{msg:?}");
    }

    #[tokio::test]
    async fn run_container_with_a_bogus_image_errors_as_a_task_error() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::RunContainer {
                opts: Box::new(crate::domain::docker::images::ContainerOpts {
                    image: "tvdocker-no-such-image".into(),
                    ..Default::default()
                }),
            })
            .expect("alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("answered")
            .expect("open");
        assert!(matches!(msg, AppMsg::TaskError(_)), "{msg:?}");
    }

    #[tokio::test]
    async fn start_shell_on_a_bogus_container_fails_into_the_phase_channel() {
        let (_app_tx, _app_rx, _action_tx, _action_rx, task_tx) = worker();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<PtyMsg>(8);
        let (_input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (_resize_tx, resize_rx) = tokio::sync::watch::channel((80u16, 24u16));
        task_tx
            .send(TaskReq::StartShell {
                id: "tvdocker-no-such-container".into(),
                shell: "sh".into(),
                user: String::new(),
                workdir: String::new(),
                out_tx,
                input_rx,
                resize_rx,
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), out_rx.recv())
            .await
            .expect("worker answered")
            .expect("channel open");
        match msg {
            PtyMsg::Phase(PtyPhase::Failed { error }) => assert!(
                error.contains("create_exec") || error.contains("docker"),
                "the widget shows exec failures in its status row: {error}"
            ),
            other => panic!("expected a Failed phase, got {other:?}"),
        }
    }

    /// A volume's browsed paths are LOGICAL
    /// ("/sub/f") but the helper mounts the volume at `/volume_data`, so every
    /// in-container use must map them (listing already does via
    /// `list_volume_dir`). Host/Container use the path unchanged.
    #[test]
    fn remote_paths_map_a_volume_into_the_helper_mount() {
        let vol = crate::core::ExplorerTarget::Volume("data".into());
        assert_eq!(remote_path(&vol, "/sub/f"), "/volume_data/sub/f");
        assert_eq!(remote_path(&vol, "/"), "/volume_data");
        assert_eq!(
            remote_paths(&vol, &["/sub/f".into(), "/g".into()]),
            vec![
                "/volume_data/sub/f".to_string(),
                "/volume_data/g".to_string()
            ]
        );
        let c = crate::core::ExplorerTarget::Container("id".into());
        assert_eq!(remote_path(&c, "/sub/f"), "/sub/f");
        assert_eq!(remote_path(&crate::core::ExplorerTarget::Host, "/x"), "/x");
        assert_eq!(remote_paths(&c, &["/a".into()]), vec!["/a".to_string()]);
    }

    /// Needs a live daemon AND creates a
    /// volume + helper. Pins the volume PATH MAPPING through the REAL worker
    /// arms — listing maps in `list_volume_dir`, but create/rename/delete/
    /// copy-from must map too (they did not, so they hit paths that do not
    /// exist in the helper). No hermetic test can cover this (host-safety:
    /// volume requests create a helper container), which is why it is an
    /// ignored probe. Run with:
    /// `./dcargo test -- --ignored volume_mutations_map_paths_live`
    #[tokio::test]
    #[ignore = "needs a Docker daemon"]
    async fn volume_mutations_map_paths_live() {
        use crate::core::ExplorerTarget;
        let volume = format!("tvdocker-volprobe-{}", std::process::id());
        let docker = client::connect().expect("daemon reachable");
        docker
            .create_volume(bollard::volume::CreateVolumeOptions {
                name: volume.clone(),
                ..Default::default()
            })
            .await
            .expect("create volume");
        let target = ExplorerTarget::Volume(volume.clone());

        // Seed /sub through a mapped helper path, then drive the ARMS with
        // LOGICAL paths and confirm the volume (not the helper overlay) moved.
        let id = super::super::explorer::ensure_volume_helper(&docker, &volume)
            .await
            .expect("helper");
        super::super::explorer::create_in_container(
            &docker,
            &id,
            &super::super::explorer::volume_container_path("/sub"),
            true,
        )
        .await
        .expect("seed dir");

        create_for(&target, "/sub/new.txt", false)
            .await
            .expect("create via arm");
        let rows = super::super::explorer::list_volume_dir(&docker, &volume, "/sub")
            .await
            .expect("list");
        assert!(
            rows.iter().any(|r| r.name == "new.txt"),
            "create_for mapped its path: {rows:?}"
        );

        rename_for(&target, "/sub/new.txt", "/sub/renamed.txt")
            .await
            .expect("rename via arm");
        let rows = super::super::explorer::list_volume_dir(&docker, &volume, "/sub")
            .await
            .expect("list");
        assert!(
            rows.iter().any(|r| r.name == "renamed.txt"),
            "rename_for mapped both paths: {rows:?}"
        );

        let out = std::env::temp_dir().join(format!("tvdocker-volprobe-{}", std::process::id()));
        std::fs::create_dir_all(&out).expect("mkdir");
        copy_for(
            &target,
            false,
            &["/sub/renamed.txt".to_string()],
            out.to_str().expect("utf8"),
        )
        .await
        .expect("copy-from via arm");
        assert!(
            out.join("renamed.txt").is_file(),
            "copy_for mapped its source path"
        );

        mutate_for(&target, vec!["/sub/renamed.txt".to_string()], true)
            .await
            .expect("delete via arm");
        let rows = super::super::explorer::list_volume_dir(&docker, &volume, "/sub")
            .await
            .expect("list");
        assert!(
            !rows.iter().any(|r| r.name == "renamed.txt"),
            "delete mapped its path: {rows:?}"
        );

        // Tear down: stop the helper, then drop the volume once Docker's
        // async auto-remove has released it (a probe must not leave junk).
        super::super::explorer::remove_volume_helper(&docker, &volume)
            .await
            .ok();
        for _ in 0..30 {
            if docker
                .remove_volume(
                    &volume,
                    Some(bollard::volume::RemoveVolumeOptions { force: true }),
                )
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        std::fs::remove_dir_all(&out).ok();
    }

    #[tokio::test]
    async fn explorer_list_bogus_container_errors() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::ExplorerList {
                key: "k".into(),
                target: crate::core::ExplorerTarget::Container("tvdocker-no-such".into()),
                path: "/".into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("answered")
            .expect("open");
        assert!(matches!(msg, AppMsg::TaskError(_)), "{msg:?}");
    }

    #[tokio::test]
    async fn explorer_list_missing_host_path_errors() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::ExplorerList {
                key: "k".into(),
                target: crate::core::ExplorerTarget::Host,
                path: "/tvdocker-missing-dir".into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("answered")
            .expect("open");
        assert!(matches!(msg, AppMsg::TaskError(_)), "{msg:?}");
    }

    #[tokio::test]
    async fn explorer_list_host_dir_publishes_a_listing() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        let dir = std::env::temp_dir().join(format!("tvdocker-worker-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("f.txt"), b"x").expect("write");
        task_tx
            .send(TaskReq::ExplorerList {
                key: "k".into(),
                target: crate::core::ExplorerTarget::Host,
                path: dir.to_str().expect("utf8").into(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("answered")
            .expect("open");
        match msg {
            AppMsg::ExplorerDir {
                key,
                target,
                entries,
                ..
            } => {
                assert_eq!(key, "k");
                assert_eq!(target, crate::core::ExplorerTarget::Host);
                assert!(entries.iter().any(|e| e.name == "f.txt"), "{entries:?}");
            }
            other => panic!("ExplorerDir expected, got another variant: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A copy's epilogue must re-list the DESTINATION side: with
    /// `to_container == false` the `dst` is a HOST dir, so the fresh
    /// listing belongs to the host panel — keying it by the window's
    /// remote `target` listed the host path inside the container and
    /// published it to the remote slot, leaving the host list stale.
    #[test]
    fn a_copy_relists_its_destination_side() {
        use crate::core::ExplorerTarget;
        let container = ExplorerTarget::Container("c1".into());
        assert_eq!(
            copy_relist_target(&container, false),
            ExplorerTarget::Host,
            "container→host must re-list the HOST dir"
        );
        assert_eq!(
            copy_relist_target(&container, true),
            container.clone(),
            "host→container re-lists the remote dir"
        );
        let image = ExplorerTarget::Image("sha256:aaa".into());
        assert_eq!(
            copy_relist_target(&image, false),
            ExplorerTarget::Host,
            "image→host must re-list the HOST dir"
        );
    }

    /// A bogus id must fail into the diagnostics channel (the window's
    /// error phase) — read-only, so this is host-safe.
    #[tokio::test]
    async fn diagnostics_on_a_bogus_id_fails_into_the_diagnostics_channel() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::StartDiagnostics {
                id: "tvdocker-no-such-container".into(),
                run: 7,
                config: crate::domain::config::LlmConfig::default(),
            })
            .expect("worker alive");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
            .await
            .expect("worker answered")
            .expect("channel open");
        match msg {
            AppMsg::DiagnosticsError { id, run, message } => {
                assert_eq!(id, "tvdocker-no-such-container");
                assert_eq!(run, 7, "the run id is echoed back");
                assert!(!message.is_empty());
            }
            other => panic!("expected a DiagnosticsError, got {other:?}"),
        }
    }

    /// Review Focus #2 — a concurrency smoke test: a diagnostics request
    /// and an unrelated inspect request are BOTH answered. The
    /// non-blocking property itself is structural — the detached
    /// `tokio::spawn` in `spawn_diagnostics` (the `spawn_logs` precedent)
    /// — not a latency pin: with a bogus id both requests fail fast, so
    /// this test cannot time a slow provider.
    #[tokio::test]
    async fn diagnostics_does_not_block_the_task_loop() {
        let (_app_tx, mut app_rx, _action_tx, _action_rx, task_tx) = worker();
        task_tx
            .send(TaskReq::StartDiagnostics {
                id: "tvdocker-no-such-container".into(),
                run: 1,
                config: crate::domain::config::LlmConfig::default(),
            })
            .expect("alive");
        task_tx
            .send(TaskReq::Inspect {
                id: "tvdocker-no-such-container".into(),
            })
            .expect("alive");
        let mut saw_diagnostics = false;
        let mut saw_inspect = false;
        for _ in 0..2 {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(15), app_rx.recv())
                .await
                .expect("answered")
                .expect("open");
            match msg {
                AppMsg::DiagnosticsError { .. } => saw_diagnostics = true,
                AppMsg::TaskError(_) => saw_inspect = true,
                other => panic!("unexpected: {other:?}"),
            }
        }
        assert!(saw_diagnostics && saw_inspect, "both requests were served");
    }

    /// `spawn_diagnostics` is FIRE-AND-FORGET: the sync helper returns
    /// immediately, deferring the work to a spawned task, so the caller's
    /// loop is never serialized behind it. Pinned on an explicit
    /// current-thread runtime, where a spawned task cannot run before the
    /// first yield — the detached shape the smoke test above exercises.
    #[test]
    fn spawn_diagnostics_is_fire_and_forget() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        rt.block_on(async {
            let (app_tx, mut app_rx) = tokio::sync::mpsc::channel::<AppMsg>(8);
            spawn_diagnostics(
                app_tx,
                "tvdocker-no-such-container".into(),
                3,
                LlmConfig::default(),
            );
            // No yield has happened yet, so the spawned task cannot have run.
            assert!(
                app_rx.try_recv().is_err(),
                "the work must be deferred to a spawned task, not run inline"
            );
            // ...and it does run, echoing the run id, once the runtime drives it.
            let msg = tokio::time::timeout(std::time::Duration::from_secs(10), app_rx.recv())
                .await
                .expect("answered")
                .expect("open");
            assert!(
                matches!(msg, AppMsg::DiagnosticsError { run: 3, .. }),
                "expected a DiagnosticsError for run 3, got {msg:?}"
            );
        });
    }
    /// The image pane is read-only (image-explorer): the UI refuses
    /// these, and the worker refuses again — pinned here so the guarantee
    /// cannot depend on the view.
    #[tokio::test]
    async fn mutate_for_create_for_and_rename_for_reject_the_image_target() {
        let image = crate::core::ExplorerTarget::Image("sha256:abc".into());
        // Assert the GUARD'S message, not just `is_err()`: "sha256:abc" does
        // not exist, so the daemon's own "no such image" error would satisfy
        // `is_err()` with every guard deleted (review mutation M1). A daemon
        // error can never contain "read-only", so the guard becomes
        // load-bearing for this test.
        for (label, err) in [
            ("delete", mutate_for(&image, vec!["/a".into()], true).await),
            ("create", create_for(&image, "/a", true).await),
            ("rename", rename_for(&image, "/a", "/b").await),
        ] {
            let err = err.expect_err(&format!("{label} into an image must fail"));
            assert!(
                err.to_string().contains("read-only"),
                "{label} is refused by OUR guard, not the daemon: {err}"
            );
        }
    }

    /// …and copying must only ever go image → host.
    #[tokio::test]
    async fn copy_into_the_image_is_rejected() {
        let image = crate::core::ExplorerTarget::Image("sha256:abc".into());
        // Same reasoning as above: assert the guard's message, so a daemon
        // error cannot masquerade as our refusal.
        let err = copy_for(&image, true, &["/b".into()], "/from-host")
            .await
            .expect_err("host → image must fail");
        assert!(
            err.to_string().contains("copying TO the host"),
            "copy-in is refused by OUR guard, not the daemon: {err}"
        );
        // The host target is untouched by the guard.
        assert!(
            remote_id(&crate::core::ExplorerTarget::Host).await.is_ok(),
            "the host target is unaffected"
        );
    }
}
