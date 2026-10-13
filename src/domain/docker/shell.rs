//! PTY shell session : the exec wiring moved VERBATIM from
//! `examples/pty_probe.rs` so the probe and the app share one path
//!. Called by the task worker; ends when the stream
//! finishes or the window drops its receiver.

use crate::widgets::pty::{PtyMsg, PtyPhase};
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use futures_util::StreamExt;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

/// Resolve the dialog's user field the way oh-my-docker does
/// (`util::resolve_host_user`): `"host"` → the current `uid:gid` via
/// `id -u`/`id -g` (fallback `0:0`), anything else passes through
/// (`root`, `user:group`, `1000:1000`, empty = container default).
pub fn resolve_host_user(user: &str) -> String {
    if user != "host" {
        return user.to_string();
    }
    let grab = |arg: &str| {
        std::process::Command::new("id")
            .arg(arg)
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|s| s.trim().to_string())
    };
    match (grab("-u"), grab("-g")) {
        (Some(uid), Some(gid)) => format!("{uid}:{gid}"),
        _ => "0:0".to_string(),
    }
}

/// Spawn the full exec session for one `TaskReq::StartShell` (decision
/// #20): create_exec → start_exec → writer task + output pump + 120 ms
/// resize debounce → inspect_exec → `PtyPhase`. The window owns the
/// senders, so a dropped window (`out_rx` gone ⇒ `out_tx.send` fails)
/// tears the session down by itself — no StopShell message needed.
pub fn spawn_shell(
    container: String,
    shell: String,
    user: String,
    workdir: String,
    out_tx: mpsc::Sender<PtyMsg>,
    input_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    resize_rx: watch::Receiver<(u16, u16)>,
) {
    tokio::spawn(async move {
        let docker = match super::client::connect() {
            Ok(d) => d,
            Err(e) => {
                let _ = out_tx
                    .send(PtyMsg::Phase(PtyPhase::Failed {
                        error: format!("docker: {e}"),
                    }))
                    .await;
                return;
            }
        };
        let opts = CreateExecOptions::<String> {
            cmd: Some(vec![shell]),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(true),
            user: if user.is_empty() {
                None
            } else {
                Some(resolve_host_user(&user))
            },
            working_dir: if workdir.is_empty() {
                None
            } else {
                Some(workdir)
            },
            ..Default::default()
        };
        let exec_id = match docker.create_exec(&container, opts).await {
            Ok(r) => r.id,
            Err(e) => {
                let _ = out_tx
                    .send(PtyMsg::Phase(PtyPhase::Failed {
                        error: format!("create_exec: {e}"),
                    }))
                    .await;
                return;
            }
        };

        // Resize debouncer (120 ms quiet — probe verbatim): coalesce
        // drags, swallow errors (the stream stays alive).
        {
            let docker = docker.clone();
            let exec_id = exec_id.clone();
            tokio::spawn(async move {
                let mut resize_rx = resize_rx;
                loop {
                    if resize_rx.changed().await.is_err() {
                        return;
                    }
                    let (mut w, mut h) = *resize_rx.borrow_and_update();
                    while tokio::time::timeout(Duration::from_millis(120), resize_rx.changed())
                        .await
                        .is_ok_and(|r| r.is_ok())
                    {
                        let (w2, h2) = *resize_rx.borrow_and_update();
                        (w, h) = (w2, h2);
                    }
                    let _ = docker
                        .resize_exec(
                            &exec_id,
                            ResizeExecOptions {
                                height: h,
                                width: w,
                            },
                        )
                        .await;
                }
            });
        }

        let start = match docker.start_exec(&exec_id, None::<StartExecOptions>).await {
            Ok(r) => r,
            Err(e) => {
                let _ = out_tx
                    .send(PtyMsg::Phase(PtyPhase::Failed {
                        error: format!("start_exec: {e}"),
                    }))
                    .await;
                return;
            }
        };
        match start {
            StartExecResults::Attached { mut output, input } => {
                let _ = out_tx.send(PtyMsg::Note("attached".to_string())).await;
                // Keystrokes → stdin (its own task; ends when the window
                // drops the sender side of `input_rx`'s pair).
                tokio::spawn(async move {
                    let mut input_rx = input_rx;
                    let mut input = input;
                    while let Some(bytes) = input_rx.recv().await {
                        if input.write_all(&bytes).await.is_err() || input.flush().await.is_err() {
                            break;
                        }
                    }
                });
                // stdout/stderr → the window. A failed send means the
                // window is gone: drop the attach (self-teardown).
                loop {
                    match output.next().await {
                        Some(Ok(LogOutput::StdOut { message }))
                        | Some(Ok(LogOutput::StdErr { message }))
                        | Some(Ok(LogOutput::Console { message })) => {
                            if out_tx.send(PtyMsg::Bytes(message.to_vec())).await.is_err() {
                                return;
                            }
                        }
                        Some(Ok(LogOutput::StdIn { .. })) => {}
                        Some(Err(e)) => {
                            let _ = out_tx
                                .send(PtyMsg::Phase(PtyPhase::Failed {
                                    error: format!("stream: {e}"),
                                }))
                                .await;
                            break;
                        }
                        None => break,
                    }
                }
                match docker.inspect_exec(&exec_id).await {
                    Ok(info) if info.running == Some(true) => {
                        let _ = out_tx
                            .send(PtyMsg::Phase(PtyPhase::Exited { code: None }))
                            .await;
                    }
                    Ok(info) => {
                        let _ = out_tx
                            .send(PtyMsg::Phase(PtyPhase::Exited {
                                code: info.exit_code,
                            }))
                            .await;
                    }
                    Err(e) => {
                        let _ = out_tx
                            .send(PtyMsg::Phase(PtyPhase::Failed {
                                error: format!("inspect: {e}"),
                            }))
                            .await;
                    }
                }
            }
            _ => {
                let _ = out_tx
                    .send(PtyMsg::Phase(PtyPhase::Failed {
                        error: "exec was detached".to_string(),
                    }))
                    .await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_host_user_passthrough_and_host_resolution() {
        assert_eq!(resolve_host_user(""), "");
        assert_eq!(resolve_host_user("root"), "root");
        assert_eq!(resolve_host_user("1000:1000"), "1000:1000");
        let host = resolve_host_user("host");
        assert!(
            host.contains(':'),
            "uid:gid shape from `id -u`/`id -g`: {host}"
        );
    }
}
