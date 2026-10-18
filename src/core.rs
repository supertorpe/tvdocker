//! Shared application state : background tasks publish [`AppMsg`]
//! through the bridge; views read [`AppCore`]; user intent flows back as
//! [`ActionReq`]. Tasks never touch views or `AppCore` directly.
use crate::domain::docker::containers::ContainerSummary;
use crate::domain::docker::details::Details;
use crate::domain::docker::events::DockerEvent;
use crate::domain::docker::images::ImageEntry;
use crate::domain::docker::logs::LogLine;
use crate::domain::docker::networks::NetworkEntry;
use crate::domain::docker::statistics::StatEntry;
use crate::domain::docker::volumes::VolumeEntry;
use crate::widgets::pty::PtyMsg;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio::sync::watch;

/// Docker connection state (status right side, §10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    /// Startup, before the first list completes.
    Checking,
    /// Listing succeeds.
    Up,
    /// Listing/connect failed (text shown amber,).
    Down { error: String },
}

/// Which resource a pending delete targets (one confirm flow,
/// four resources + the two batch image sweeps). Carried beside
/// `pending_targets` so the desktop's confirm/finish can map ids → the
/// right `TaskReq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PendingKind {
    #[default]
    Containers,
    Images,
    Networks,
    Volumes,
    /// Batch sweep — no targets, fixed confirm text.
    DanglingImages,
    /// Batch prune — no targets, fixed confirm text.
    PruneImages,
}

/// Which side of an Explorer window a request/response belongs to.
/// `Host` is the local fs; the remote side is either a
/// container (exec) or a volume (via the helper container).
#[derive(Clone, Debug, PartialEq)]
pub enum ExplorerTarget {
    Host,
    Container(String),
    Volume(String),
    /// An image's filesystem, reached through a helper container created
    /// from that image (image-explorer). READ-ONLY.
    Image(String),
}

/// One panel's last listing: the path it was taken at plus a
/// rev the window diffs on (the Details `details_rev` pattern); the
/// path also lets the window DROP a stale response.
#[derive(Clone, Debug, Default)]
pub struct ExplorerListing {
    pub path: String,
    pub rev: u64,
    pub entries: Vec<crate::domain::docker::explorer::ExplorerEntry>,
}

/// One-shot work for the task worker (an mpsc message, so
/// it needs NO `Clone`; tests match it with `matches!`).
pub enum TaskReq {
    /// Re-inspect → `AppMsg::Details`.
    Inspect { id: String },
    /// Remove one container; the worker sends `PollNow` on success.
    RemoveContainer { id: String },
    /// Start/stop one container — the DESKTOP filters targets by
    /// state before sending (bollard errors on no-ops).
    SetContainerState { id: String, start: bool },
    /// Remove one image by id; the worker sends `PollNow` on success.
    RemoveImage { id: String },
    /// Remove every dangling image; publishes `AppMsg::Status` with the
    /// count (host-mutating — never driven by a hermetic worker test).
    RemoveDanglingImages,
    /// Prune unused images; publishes `AppMsg::Status` with count + space
    /// reclaimed (host-mutating — never driven by a hermetic worker test).
    PruneImages,
    /// Create + start a container from the run form;
    /// `Box`ed so the enum stays small (clippy `large_enum_variant`).
    RunContainer {
        opts: Box<crate::domain::docker::images::ContainerOpts>,
    },
    /// Remove one network by id.
    RemoveNetwork { id: String },
    /// Remove one volume by NAME.
    RemoveVolume { name: String },
    /// List one Explorer panel. `key` = window identity
    /// (container id or `vol:<name>`); the response is keyed by it.
    ExplorerList {
        key: String,
        target: crate::core::ExplorerTarget,
        path: String,
    },
    /// Copy the items to the OTHER side of the window:
    /// `to_container = true` uploads to the remote target's dst path,
    /// `false` downloads into the host dst dir.
    ExplorerCopy {
        key: String,
        target: crate::core::ExplorerTarget,
        to_container: bool,
        items: Vec<String>,
        dst: String,
    },
    /// Delete the paths on the target side (HOST-MUTATING on Host —
    /// never driven by a hermetic worker test).
    ExplorerDelete {
        key: String,
        target: crate::core::ExplorerTarget,
        paths: Vec<String>,
    },
    /// mkdir/touch `path` on the target side.
    ExplorerCreate {
        key: String,
        target: crate::core::ExplorerTarget,
        path: String,
        is_dir: bool,
    },
    /// mv `old` → `new` on the target side.
    ExplorerRename {
        key: String,
        target: crate::core::ExplorerTarget,
        old: String,
        new: String,
    },
    /// Stop the volume helper when its window closes
    /// (HOST-MUTATING).
    RemoveVolumeHelper { name: String },
    /// Stop the image helper when its window closes (image-explorer;
    /// HOST-MUTATING).
    RemoveImageHelper { image: String },
    /// Analyze one image's layers (image-layer explorer). `key` is the
    /// image id; the response is keyed by it.
    AnalyzeImageLayers { key: String, image: String },
    /// Fresh-tail log session for one container. The
    /// flag was created UI-side and already sits in the core buffer;
    /// the worker exits when it flips.
    StartLogs { id: String, stop: Arc<AtomicBool> },
    /// Open a PTY exec. Channels are created by the desktop: the
    /// worker owns the receivers, the window owns the senders
    /// (impossible on the broadcast `ActionReq`).
    StartShell {
        id: String,
        shell: String,
        user: String,
        workdir: String,
        out_tx: mpsc::Sender<PtyMsg>,
        input_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        resize_rx: watch::Receiver<(u16, u16)>,
    },
    /// Run AI diagnostics for one container. `run` echoes the
    /// `AppCore::start_diagnostics` sequence so a stale response drops.
    /// The config is re-read by the desktop each run .
    StartDiagnostics {
        id: String,
        run: u64,
        config: crate::domain::config::LlmConfig,
    },
    /// Download the release archive and replace the running binary
    /// (HOST-MUTATING — never driven by a hermetic worker test past a
    /// refused connection). The worker reports via the status latch.
    UpdateInstall { version: String, url: String },
}

/// One buffered event: `seq` is assigned at ingest and never
/// renumbered, so row ids stay stable when old events are trimmed.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRow {
    pub seq: u64,
    pub event: DockerEvent,
}
/// Everything a background task may publish . `PartialEq`
/// only — `StatEntry` carries `f64`, so `Eq` is gone;
/// every existing `assert_eq!` keeps working.
#[derive(Clone, Debug, PartialEq)]
pub enum AppMsg {
    Containers(Vec<ContainerSummary>),
    Images(Vec<ImageEntry>),
    Networks(Vec<NetworkEntry>),
    Volumes(Vec<VolumeEntry>),
    Statistics(Vec<StatEntry>),
    Events(Vec<DockerEvent>),
    Connection(Connection),
    /// One container's fresh log batch — already ANSI-stripped;
    /// dropped when the session no longer exists.
    LogLines {
        id: String,
        lines: Vec<LogLine>,
    },
    /// Log poll health flip → the window's `(reconnecting…)` info state
    /// (`ok` changes only ⇒ no refresh churn).
    LogStatus {
        id: String,
        ok: bool,
    },
    /// A rendered inspect view, keyed by container id
    /// (amended for the ×N Details windows).
    Details {
        id: String,
        details: Details,
    },
    /// A latched batch-result info line (survives until the
    /// next message; rendered without the ⚠ marker).
    Status(String),
    /// One Explorer panel's fresh listing, keyed by the
    /// window `key` + side.
    ExplorerDir {
        key: String,
        target: crate::core::ExplorerTarget,
        path: String,
        entries: Vec<crate::domain::docker::explorer::ExplorerEntry>,
    },
    /// One image's finished layer analysis, keyed by image id (boxed: a
    /// `Layer` carries a whole `Tree`, so a bare enum would trip
    /// `large_enum_variant`).
    ImageLayers {
        key: String,
        analysis: Box<crate::domain::layers::Analysis>,
    },
    /// Bytes of the `docker save` stream so far, keyed by image id.
    LayerProgress {
        key: String,
        bytes: u64,
    },
    /// A background task failed (§10: never a modal).
    TaskError(String),
    /// A diagnostics run's phase change.
    DiagnosticsPhase {
        id: String,
        run: u64,
        phase: DiagnosticsPhase,
    },
    /// The whole analysis text for a run (buffered).
    DiagnosticsAnalysis {
        id: String,
        run: u64,
        text: String,
    },
    /// The whole playbook text for a run (buffered).
    DiagnosticsPlaybook {
        id: String,
        run: u64,
        text: String,
    },
    /// The run completed cleanly.
    DiagnosticsDone {
        id: String,
        run: u64,
    },
    /// The run failed: the window shows it AND the status line latches
    /// it (§10) — never a modal.
    DiagnosticsError {
        id: String,
        run: u64,
        message: String,
    },
}

/// Ring-buffer cap for the logs stream (10k discipline;
/// "Live scrolling log view"). Trim removes the FRONT —
/// newest events are the product.
pub const LOGS_CAP: usize = 10_000;

/// One container's log session.
#[derive(Debug)]
pub struct LogBuffer {
    /// Display-text lines, oldest first (ANSI stripped at ingest).
    pub lines: Vec<LogLine>,
    /// Total lines ever ingested THIS session — monotonic across
    /// trims; the window's incremental sync keys on it.
    pub ingested: u64,
    /// Set true by `LogsWindow::drop`; the worker's task exits.
    pub stop: Arc<AtomicBool>,
    /// `false` ⇒ the info row shows ` (reconnecting…)`.
    pub ok: bool,
}

/// One diagnostics run's phase. `Error` carries the message
/// shown in the window (and latched on the status line — §10).
#[derive(Clone, Debug, PartialEq)]
pub enum DiagnosticsPhase {
    /// Context is being collected (inspect + logs + stats).
    Collecting,
    /// The LLM request is in flight.
    Analyzing,
    /// Analysis + playbook are complete.
    Done,
    /// The run failed.
    Error(String),
}

/// One container's diagnostics run (×N). `run` is the monotonic
/// per-container sequence: messages carrying a stale `run` are dropped
/// (a re-run superseded them).
#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticsState {
    pub container_id: String,
    pub run: u64,
    pub phase: DiagnosticsPhase,
    /// Whole analysis text (buffered — phase-1 ruling 3); replaced, not
    /// appended, on a new run.
    pub analysis: String,
    /// Whole playbook text.
    pub playbook: String,
}

/// Everything a user may intend from the UI .
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionReq {
    /// Poll immediately instead of waiting for the interval tick.
    PollNow,
}

/// Ring-buffer cap for the events stream (10k discipline;
/// "Live scrolling event stream"). Trim removes the FRONT —
/// newest events are the product.
pub const EVENTS_CAP: usize = 10_000;

/// The shared state the bridge drains into and every view renders from.
#[derive(Debug)]
pub struct AppCore {
    /// The eight 3A fields unchanged…
    pub containers: Vec<ContainerSummary>,
    pub images: Vec<ImageEntry>,
    pub networks: Vec<NetworkEntry>,
    pub volumes: Vec<VolumeEntry>,
    pub statistics: Vec<StatEntry>,
    /// Newest LAST; the Events window maps it newest-first.
    pub events: Vec<EventRow>,
    /// Next `EventRow::seq` (monotonic, starts at 1).
    pub event_seq: u64,
    pub connection: Connection,
    /// Bumped on every list refresh; views skip redundant re-maps.
    pub revision: u64,
    /// Latest inspect render per container id (amended for
    /// the ×N Details windows); an absent entry = loading.
    pub details: HashMap<String, Details>,
    /// Bumped per `AppMsg::Details` — windows diff on it.
    pub details_rev: u64,
    /// Per-container diagnostics runs (×N); an absent entry
    /// means no run has started for that container.
    pub diagnostics: HashMap<String, DiagnosticsState>,
    /// Bumped per diagnostics message — windows diff on it.
    pub diagnostics_rev: u64,
    /// Next per-container run sequence (monotonic; guards stale runs).
    pub diagnostics_next_run: u64,
    /// Open (or previously opened) log sessions by container id.
    pub logs: HashMap<String, LogBuffer>,
    /// Latched background-task error for the status right side;
    /// cleared by `Connection(Up)`.
    pub last_error: Option<String>,
    /// Latched info message (batch-result slot); cleared by
    /// `Connection(Up)` like `last_error`.
    pub info: Option<String>,
    /// Targets queued by the focused window for the desktop's next
    /// intercepted command.
    pub pending_targets: Vec<String>,
    /// Which resource those targets belong to (the desktop's
    /// confirm/finish maps ids → `TaskReq` by kind).
    pub pending_kind: PendingKind,
    /// Last Explorer listing per window+side, keyed `"{key}:{'h'|'r'}"`;
    /// the window applies only path-matching entries.
    pub explorer: HashMap<String, ExplorerListing>,
    /// Finished (or in-flight) analyses by image id; `None` = loading.
    pub image_layers: HashMap<String, Option<crate::domain::layers::Analysis>>,
    /// `docker save` byte progress by image id (absent = no run).
    pub layers_progress: HashMap<String, u64>,
    /// Bumped per layer message (progress AND results) — the noisy
    /// revision, kept for progress consumers.
    pub layers_rev: u64,
    /// Bumped ONLY by `AppMsg::ImageLayers` — the data revision the
    /// Layers window gates its (expensive) analysis re-read on, so a
    /// per-chunk progress tick never deep-clones the per-layer trees.
    pub layers_data_rev: u64,
}

impl AppCore {
    pub fn new() -> Self {
        Self {
            containers: Vec::new(),
            images: Vec::new(),
            networks: Vec::new(),
            volumes: Vec::new(),
            statistics: Vec::new(),
            events: Vec::new(),
            event_seq: 0,
            connection: Connection::Checking,
            revision: 0,
            details: HashMap::new(),
            details_rev: 0,
            diagnostics: HashMap::new(),
            diagnostics_rev: 0,
            diagnostics_next_run: 0,
            logs: HashMap::new(),
            last_error: None,
            info: None,
            pending_targets: Vec::new(),
            pending_kind: PendingKind::default(),
            explorer: HashMap::new(),
            image_layers: HashMap::new(),
            layers_progress: HashMap::new(),
            layers_rev: 0,
            layers_data_rev: 0,
        }
    }

    /// Begin a diagnostics run for `id`: install a fresh `Collecting`
    /// state and return the run id for the `TaskReq`.
    pub fn start_diagnostics(&mut self, id: &str) -> u64 {
        self.diagnostics_next_run += 1;
        let run = self.diagnostics_next_run;
        self.diagnostics.insert(
            id.to_string(),
            DiagnosticsState {
                container_id: id.to_string(),
                run,
                phase: DiagnosticsPhase::Collecting,
                analysis: String::new(),
                playbook: String::new(),
            },
        );
        self.diagnostics_rev += 1;
        self.revision += 1;
        run
    }

    /// `(running, total)` for the status-line counts .
    pub fn counts(&self) -> (usize, usize) {
        let total = self.containers.len();
        let running = self
            .containers
            .iter()
            .filter(|c| c.state == "running")
            .count();
        (running, total)
    }

    /// Queue the ids + the resource kind the desktop's next intercepted
    /// `CMD_DELETE` should act on.
    pub fn set_pending(&mut self, ids: Vec<String>, kind: PendingKind) {
        self.pending_targets = ids;
        self.pending_kind = kind;
    }

    /// Take the queued kind and reset it — a stale kind must never fire
    /// (mirrors `take_pending`'s destructive discipline).
    pub fn take_kind(&mut self) -> PendingKind {
        std::mem::take(&mut self.pending_kind)
    }

    /// Take the queued ids (desktop side) — destructive, so a stale
    /// queue can never fire twice.
    pub fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_targets)
    }

    /// Apply one bridged message (the reducer,).
    pub fn apply(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Containers(list) => {
                self.containers = list;
                self.revision += 1;
            }
            AppMsg::Images(list) => {
                self.images = list;
                self.revision += 1;
            }
            AppMsg::Networks(list) => {
                self.networks = list;
                self.revision += 1;
            }
            AppMsg::Volumes(list) => {
                self.volumes = list;
                self.revision += 1;
            }
            AppMsg::Statistics(list) => {
                self.statistics = list;
                self.revision += 1;
            }
            AppMsg::Events(batch) => {
                if batch.is_empty() {
                    return; // never emit a phantom refresh
                }
                for event in batch {
                    self.event_seq += 1;
                    self.events.push(EventRow {
                        seq: self.event_seq,
                        event,
                    });
                }
                let excess = self.events.len().saturating_sub(EVENTS_CAP);
                if excess > 0 {
                    self.events.drain(..excess); // trim the FRONT only
                }
                self.revision += 1;
            }
            AppMsg::Connection(c) => {
                let recovered = c == Connection::Up;
                self.connection = c;
                if recovered {
                    self.last_error = None;
                    self.info = None;
                }
            }
            AppMsg::LogLines { id, lines } => {
                if lines.is_empty() {
                    return; // no phantom refresh (3A's events rule, applied)
                }
                if let Some(buf) = self.logs.get_mut(&id) {
                    for line in lines {
                        buf.lines.push(line);
                        buf.ingested += 1;
                    }
                    let excess = buf.lines.len().saturating_sub(LOGS_CAP);
                    self.revision += 1;
                    if excess > 0 {
                        buf.lines.drain(..excess); // trim the FRONT
                    }
                }
                // Unknown session (window closed mid-batch) => drop silently.
            }
            AppMsg::LogStatus { id, ok } => {
                if let Some(buf) = self.logs.get_mut(&id)
                    && buf.ok != ok
                {
                    buf.ok = ok;
                    self.revision += 1;
                }
            }
            AppMsg::Details { id, details } => {
                self.details.insert(id, details);
                self.details_rev += 1;
                self.revision += 1;
            }
            AppMsg::Status(msg) => {
                self.info = Some(msg);
                self.revision += 1;
            }
            AppMsg::ExplorerDir {
                key,
                target,
                path,
                entries,
            } => {
                let side = match target {
                    ExplorerTarget::Host => "h",
                    _ => "r",
                };
                let slot = self.explorer.entry(format!("{key}:{side}")).or_default();
                slot.path = path;
                slot.entries = entries;
                slot.rev += 1;
                self.revision += 1;
            }
            AppMsg::ImageLayers { key, analysis } => {
                self.image_layers.insert(key.clone(), Some(*analysis));
                self.layers_progress.remove(&key);
                self.layers_rev += 1;
                self.layers_data_rev += 1; // the analysis itself changed
                self.revision += 1;
            }
            AppMsg::LayerProgress { key, bytes } => {
                self.layers_progress.insert(key, bytes);
                self.layers_rev += 1;
                self.revision += 1;
            }
            AppMsg::TaskError(msg) => {
                self.last_error = Some(msg);
                self.revision += 1;
            }
            AppMsg::DiagnosticsPhase { id, run, phase } => {
                if let Some(state) = self.diagnostics.get_mut(&id)
                    && state.run == run
                {
                    state.phase = phase;
                    self.diagnostics_rev += 1;
                    self.revision += 1;
                }
            }
            AppMsg::DiagnosticsAnalysis { id, run, text } => {
                if let Some(state) = self.diagnostics.get_mut(&id)
                    && state.run == run
                {
                    state.analysis = text; // replace, never append (ruling 5)
                    self.diagnostics_rev += 1;
                    self.revision += 1;
                }
            }
            AppMsg::DiagnosticsPlaybook { id, run, text } => {
                if let Some(state) = self.diagnostics.get_mut(&id)
                    && state.run == run
                {
                    state.playbook = text;
                    self.diagnostics_rev += 1;
                    self.revision += 1;
                }
            }
            AppMsg::DiagnosticsDone { id, run } => {
                if let Some(state) = self.diagnostics.get_mut(&id)
                    && state.run == run
                {
                    state.phase = DiagnosticsPhase::Done;
                    self.diagnostics_rev += 1;
                    self.revision += 1;
                }
            }
            AppMsg::DiagnosticsError { id, run, message } => {
                if let Some(state) = self.diagnostics.get_mut(&id)
                    && state.run == run
                {
                    state.phase = DiagnosticsPhase::Error(message.clone());
                    self.diagnostics_rev += 1;
                }
                // §10: the affected window shows it (phase) AND the
                // status line latches it. Latch even if the run is
                // stale — the failure is real either way.
                self.last_error = Some(message);
                self.revision += 1;
            }
        }
    }

    /// Start (or restart) a container's log session: fresh lines, healthy,
    /// a NEW stop flag (the desktop owns the flag so it
    /// exists before any data arrives). A previous session's flag flips
    /// first, so its worker exits instead of leaking on reopen.
    pub fn set_logs_session(&mut self, id: &str) {
        if let Some(old) = self.logs.get(id) {
            old.stop.store(true, Ordering::SeqCst);
        }
        self.logs.insert(
            id.to_string(),
            LogBuffer {
                lines: Vec::new(),
                ingested: 0,
                stop: Arc::new(AtomicBool::new(false)),
                ok: true,
            },
        );
    }
}

impl Default for AppCore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_line(msg: &str) -> crate::domain::docker::logs::LogLine {
        crate::domain::docker::logs::LogLine {
            timestamp: "2026-10-08T10:00:00.000000000Z".into(),
            message: msg.into(),
        }
    }

    #[test]
    fn pending_targets_round_trip_through_set_and_take() {
        // the window queues, the desktop takes — an empty
        // take must not fire an action with phantom targets.
        let mut core = AppCore::new();
        assert!(core.take_pending().is_empty(), "nothing queued yet");
        core.set_pending(vec!["id1".into(), "id2".into()], PendingKind::Containers);
        assert_eq!(core.take_pending(), vec!["id1", "id2"]);
        assert!(
            core.take_pending().is_empty(),
            "take is destructive — no stale second action"
        );
    }

    #[test]
    fn explorer_listings_land_per_side_and_bump_their_rev() {
        use crate::domain::docker::explorer::ExplorerEntry;
        let mut core = AppCore::new();
        let e = |name: &str| ExplorerEntry {
            name: name.into(),
            is_dir: false,
            size: 1,
            modified: String::new(),
            permissions: String::new(),
        };
        core.apply(AppMsg::ExplorerDir {
            key: "c1".into(),
            target: ExplorerTarget::Host,
            path: "/home/u".into(),
            entries: vec![e("a")],
        });
        core.apply(AppMsg::ExplorerDir {
            key: "c1".into(),
            target: ExplorerTarget::Container("c1".into()),
            path: "/app".into(),
            entries: vec![e("b")],
        });
        let host = core.explorer.get("c1:h").expect("host slot");
        assert_eq!(host.path, "/home/u");
        assert_eq!(host.entries[0].name, "a");
        assert_eq!(host.rev, 1);
        let remote = core.explorer.get("c1:r").expect("remote slot");
        assert_eq!(remote.path, "/app");
        assert_eq!(remote.rev, 1);
        // A second response for the same side bumps the rev (the window
        // re-renders on it) and replaces the entries.
        core.apply(AppMsg::ExplorerDir {
            key: "c1".into(),
            target: ExplorerTarget::Host,
            path: "/home/u/sub".into(),
            entries: vec![e("z")],
        });
        let host = core.explorer.get("c1:h").expect("host slot");
        assert_eq!(host.rev, 2);
        assert_eq!(host.path, "/home/u/sub");
        assert_eq!(host.entries.len(), 1);
    }

    #[test]
    fn image_layers_are_cached_by_key_and_clear_the_progress() {
        use crate::domain::layers::Analysis;
        let mut core = AppCore::new();
        core.apply(AppMsg::LayerProgress {
            key: "sha256:aaa".into(),
            bytes: 4096,
        });
        assert_eq!(core.layers_rev, 1);
        let rev = core.revision;
        core.apply(AppMsg::ImageLayers {
            key: "sha256:aaa".into(),
            analysis: Box::new(Analysis::default()),
        });
        assert!(core.image_layers["sha256:aaa"].is_some());
        assert!(
            !core.layers_progress.contains_key("sha256:aaa"),
            "the byte counter goes away once the result lands"
        );
        assert_eq!(core.layers_rev, 2);
        assert!(core.revision > rev, "the REFRESH broadcast fires");
    }

    /// Finding 1's revision split: `layers_rev` is the noisy progress rev,
    /// `layers_data_rev` moves ONLY when a finished analysis lands — the
    /// Layers window gates its (expensive) re-read on the latter, so a
    /// progress tick can never trigger a deep clone of the per-layer trees.
    #[test]
    fn progress_bumps_only_the_progress_rev_while_image_layers_bumps_both() {
        use crate::domain::layers::Analysis;
        let mut core = AppCore::new();
        assert_eq!(core.layers_rev, 0);
        assert_eq!(core.layers_data_rev, 0);
        core.apply(AppMsg::LayerProgress {
            key: "sha256:aaa".into(),
            bytes: 4096,
        });
        assert_eq!(core.layers_rev, 1, "progress bumps the progress rev");
        assert_eq!(
            core.layers_data_rev, 0,
            "…but never the data rev (no window rebuild)"
        );
        core.apply(AppMsg::ImageLayers {
            key: "sha256:aaa".into(),
            analysis: Box::new(Analysis::default()),
        });
        assert_eq!(core.layers_rev, 2);
        assert_eq!(core.layers_data_rev, 1, "a finished analysis bumps both");
    }

    #[test]
    fn a_layer_progress_message_does_not_overwrite_a_finished_analysis() {
        use crate::domain::layers::Analysis;
        let mut core = AppCore::new();
        core.apply(AppMsg::ImageLayers {
            key: "sha256:aaa".into(),
            analysis: Box::new(Analysis::default()),
        });
        core.apply(AppMsg::LayerProgress {
            key: "sha256:aaa".into(),
            bytes: 1,
        });
        assert!(core.image_layers["sha256:aaa"].is_some());
        assert_eq!(core.layers_progress["sha256:aaa"], 1);
    }

    #[test]
    fn set_pending_carries_the_kind_and_take_kind_resets_it() {
        let mut core = AppCore::new();
        core.set_pending(vec!["id-a".into()], PendingKind::Images);
        assert_eq!(core.pending_kind, PendingKind::Images);
        assert_eq!(core.take_kind(), PendingKind::Images);
        assert_eq!(
            core.pending_kind,
            PendingKind::Containers,
            "take_kind resets to the default (a stale kind must never fire)"
        );
    }

    #[test]
    fn log_lines_append_trim_the_front_and_count_ingested() {
        let mut core = AppCore::new();
        core.set_logs_session("c1");
        core.apply(AppMsg::LogLines {
            id: "c1".into(),
            lines: vec![log_line("a"), log_line("b")],
        });
        assert_eq!(core.logs["c1"].lines.len(), 2);
        assert_eq!(core.logs["c1"].ingested, 2);
        assert_eq!(core.revision, 1);

        // Over the cap in one message: trim the FRONT, keep `ingested` total.
        core.apply(AppMsg::LogLines {
            id: "c1".into(),
            lines: (0..LOGS_CAP).map(|i| log_line(&format!("x{i}"))).collect(),
        });
        assert_eq!(core.logs["c1"].lines.len(), LOGS_CAP, "hard cap");
        assert_eq!(core.logs["c1"].lines[0].message, "x0", "oldest evicted");
        assert_eq!(
            core.logs["c1"].ingested,
            2 + LOGS_CAP as u64,
            "ingested is monotonic — window sync keys on it"
        );
    }

    #[test]
    fn log_lines_for_an_unknown_or_empty_batch_never_phantom_bump() {
        let mut core = AppCore::new();
        core.apply(AppMsg::LogLines {
            id: "ghost".into(),
            lines: vec![log_line("x")],
        });
        core.set_logs_session("c1");
        core.apply(AppMsg::LogLines {
            id: "c1".into(),
            lines: Vec::new(),
        });
        assert_eq!(core.revision, 0, "no entry or no lines ⇒ nothing happened");
    }

    #[test]
    fn log_status_flips_only_on_change() {
        let mut core = AppCore::new();
        core.set_logs_session("c1");
        assert!(core.logs["c1"].ok, "session starts healthy");
        core.apply(AppMsg::LogStatus {
            id: "c1".into(),
            ok: false,
        });
        assert!(!core.logs["c1"].ok, "reconnect state stored");
        assert_eq!(core.revision, 1);
        core.apply(AppMsg::LogStatus {
            id: "c1".into(),
            ok: false,
        });
        assert_eq!(core.revision, 1, "same state ⇒ no refresh churn");
    }

    #[test]
    fn details_rev_moves_and_task_errors_latch_until_recover() {
        use crate::domain::docker::details::Details;
        let mut core = AppCore::new();
        assert!(
            core.details.is_empty(),
            "loading state before the first inspect"
        );
        core.apply(AppMsg::Details {
            id: "id-web".into(),
            details: Details {
                name: "web".into(),
                lines: vec![" Name".into(), " web".into()],
            },
        });
        assert_eq!(
            core.details.get("id-web").map(|d| d.name.as_str()),
            Some("web")
        );
        assert_eq!(core.details_rev, 1, "windows re-render on the rev");

        core.apply(AppMsg::TaskError(
            "inspect failed: no such container".into(),
        ));
        assert_eq!(
            core.last_error.as_deref(),
            Some("inspect failed: no such container")
        );
        core.apply(AppMsg::Connection(Connection::Up));
        assert!(
            core.last_error.is_none(),
            "a successful poll clears the latched error"
        );
        core.apply(AppMsg::Connection(Connection::Down { error: "x".into() }));
        assert!(
            core.last_error.is_none(),
            "Down carries its own reason; the latch stays cleared"
        );
    }

    #[test]
    fn status_latches_info_and_a_reconnect_clears_it() {
        let mut core = AppCore::new();
        core.apply(AppMsg::Status(
            "Pruned 3 unused images (freed 1.2GB)".into(),
        ));
        assert_eq!(
            core.info.as_deref(),
            Some("Pruned 3 unused images (freed 1.2GB)"),
            "the batch result survives until the next message"
        );
        core.apply(AppMsg::Status("Removed 1 dangling images".into()));
        assert_eq!(core.info.as_deref(), Some("Removed 1 dangling images"));
        core.apply(AppMsg::Connection(Connection::Up));
        assert!(core.info.is_none(), "reconnect clears the info slot");
        assert!(core.last_error.is_none(), "…and the error latch");
    }

    #[test]
    fn containers_apply_stores_rows_and_bumps_revision() {
        let mut core = AppCore::new();
        assert_eq!(core.revision, 0);
        core.apply(AppMsg::Containers(vec![
            summary("running"),
            summary("exited"),
        ]));
        assert_eq!(core.containers.len(), 2);
        assert_eq!(
            core.revision, 1,
            "views skip re-maps without a revision change"
        );
        core.apply(AppMsg::Containers(vec![summary("running")]));
        assert_eq!(core.revision, 2);
    }

    #[test]
    fn connection_apply_tracks_state() {
        let mut core = AppCore::new();
        assert_eq!(core.connection, Connection::Checking);
        core.apply(AppMsg::Connection(Connection::Down { error: "x".into() }));
        assert_eq!(core.connection, Connection::Down { error: "x".into() });
        core.apply(AppMsg::Connection(Connection::Up));
        assert_eq!(core.connection, Connection::Up);
    }

    #[test]
    fn counts_only_running_containers() {
        let mut core = AppCore::new();
        core.apply(AppMsg::Containers(vec![
            summary("running"),
            summary("running"),
            summary("exited"),
            summary("created"),
        ]));
        assert_eq!(
            core.counts(),
            (2, 4),
            "(running, total) for the status line"
        );
    }

    fn summary(state: &str) -> crate::domain::docker::containers::ContainerSummary {
        crate::domain::docker::containers::ContainerSummary {
            id: state.to_string(),
            name: format!("c-{state}"),
            image: "img".into(),
            state: state.into(),
            status: "Up".into(),
            ports: String::new(),
            project: String::new(),
            health: String::new(),
        }
    }

    fn docker_event(action: &str) -> crate::domain::docker::events::DockerEvent {
        crate::domain::docker::events::DockerEvent {
            timestamp: "12:00:00".into(),
            kind: "container".into(),
            action: action.into(),
            actor: "abc".into(),
        }
    }

    #[test]
    fn every_list_message_stores_its_rows_and_bumps_the_shared_revision() {
        // ONE revision for every list-ish message — windows
        // re-map cheaply on any change; per-resource revisions were rejected.
        let mut core = AppCore::new();
        core.apply(AppMsg::Images(vec![image("nginx")]));
        assert_eq!(core.images.len(), 1);
        assert_eq!(core.revision, 1);
        core.apply(AppMsg::Networks(vec![network("bridge"), network("host")]));
        assert_eq!(core.networks.len(), 2);
        assert_eq!(core.revision, 2);
        core.apply(AppMsg::Volumes(vec![volume("data")]));
        assert_eq!(core.volumes.len(), 1);
        assert_eq!(core.revision, 3);
        core.apply(AppMsg::Statistics(vec![stat("web")]));
        assert_eq!(core.statistics.len(), 1);
        assert_eq!(core.revision, 4, "views skip re-maps without a bump");
        // Connection must NOT bump: it is not row data.
        core.apply(AppMsg::Connection(Connection::Up));
        assert_eq!(core.revision, 4, "connection is not a row refresh");
    }

    #[test]
    fn events_apply_assigns_monotonic_seq_and_trims_at_the_cap() {
        let mut core = AppCore::new();
        let batch = |n: usize, from: u64| {
            (from..from + n as u64)
                .map(|i| {
                    let mut e = docker_event("start");
                    e.timestamp = format!("t{i}");
                    e
                })
                .collect::<Vec<_>>()
        };
        core.apply(AppMsg::Events(batch(3, 0)));
        let seqs: Vec<u64> = core.events.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3], "seq starts at 1, per event");

        // Over the cap in one message: trim the FRONT, never renumber.
        core.apply(AppMsg::Events(batch(EVENTS_CAP, 3)));
        assert_eq!(core.events.len(), EVENTS_CAP, "hard cap");
        assert_eq!(
            core.events.first().map(|r| r.seq),
            Some(4),
            "oldest events trimmed off the front"
        );
        assert_eq!(
            core.events.last().map(|r| r.seq),
            Some(3 + EVENTS_CAP as u64),
            "ids stable across trims — row ids never change identity"
        );
        assert!(
            core.events.windows(2).all(|w| w[0].seq < w[1].seq),
            "seq strictly increasing after a trim"
        );
        assert_eq!(core.event_seq, 3 + EVENTS_CAP as u64, "counter survives");
    }

    #[test]
    fn empty_event_batches_are_ignored() {
        let mut core = AppCore::new();
        core.apply(AppMsg::Events(vec![]));
        assert_eq!(core.revision, 0, "no phantom refresh");
        assert_eq!(core.event_seq, 0, "no phantom ids");
        assert!(core.events.is_empty());
    }

    fn image(name: &str) -> crate::domain::docker::images::ImageEntry {
        crate::domain::docker::images::ImageEntry {
            id: format!("id-{name}"),
            repository: name.into(),
            tag: "latest".into(),
            size: 1_000,
        }
    }

    fn network(name: &str) -> crate::domain::docker::networks::NetworkEntry {
        crate::domain::docker::networks::NetworkEntry {
            id: format!("id-{name}"),
            name: name.into(),
            driver: "bridge".into(),
            scope: "local".into(),
            subnet: String::new(),
            gateway: String::new(),
            containers: 0,
        }
    }

    fn volume(name: &str) -> crate::domain::docker::volumes::VolumeEntry {
        crate::domain::docker::volumes::VolumeEntry {
            name: name.into(),
            driver: "local".into(),
            mountpoint: format!("/var/lib/docker/volumes/{name}"),
            size: 0,
        }
    }

    fn stat(name: &str) -> crate::domain::docker::statistics::StatEntry {
        crate::domain::docker::statistics::StatEntry {
            name: name.into(),
            cpu_percent: 1.5,
            memory_usage: 1,
            memory_limit: 2,
            memory_percent: 50.0,
            net_rx: 0,
            net_tx: 0,
            block_read: 0,
            block_write: 0,
            pids: 1,
        }
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;

    #[test]
    fn start_installs_a_collecting_run_and_bumps_the_sequence() {
        let mut core = AppCore::new();
        let rev0 = core.diagnostics_rev;
        let run = core.start_diagnostics("id-web");
        assert!(run > 0);
        let st = core.diagnostics.get("id-web").expect("installed");
        assert_eq!(st.run, run);
        assert_eq!(st.phase, DiagnosticsPhase::Collecting);
        assert!(core.diagnostics_rev > rev0, "windows diff on the rev");
        // The next start for the SAME id is a fresh, larger run.
        let run2 = core.start_diagnostics("id-web");
        assert!(run2 > run);
    }

    #[test]
    fn a_phase_analysis_playbook_done_run_renders_in_order() {
        let mut core = AppCore::new();
        let run = core.start_diagnostics("id-web");
        core.apply(AppMsg::DiagnosticsPhase {
            id: "id-web".into(),
            run,
            phase: DiagnosticsPhase::Analyzing,
        });
        core.apply(AppMsg::DiagnosticsAnalysis {
            id: "id-web".into(),
            run,
            text: "disk full".into(),
        });
        core.apply(AppMsg::DiagnosticsPlaybook {
            id: "id-web".into(),
            run,
            text: "1. prune".into(),
        });
        core.apply(AppMsg::DiagnosticsDone {
            id: "id-web".into(),
            run,
        });
        let st = core.diagnostics.get("id-web").expect("present");
        assert_eq!(st.phase, DiagnosticsPhase::Done);
        assert_eq!(st.analysis, "disk full");
        assert_eq!(st.playbook, "1. prune");
    }

    /// Review Focus #1.
    #[test]
    fn a_stale_run_is_dropped() {
        let mut core = AppCore::new();
        let old = core.start_diagnostics("id-web");
        let new = core.start_diagnostics("id-web"); // re-run
        assert!(new > old);
        core.apply(AppMsg::DiagnosticsAnalysis {
            id: "id-web".into(),
            run: old, // the superseded run
            text: "OLD".into(),
        });
        let st = core.diagnostics.get("id-web").expect("present");
        assert!(st.analysis.is_empty(), "stale run must not write: {st:?}");
        assert_eq!(st.run, new);
    }

    /// Review Focus #3.
    #[test]
    fn an_error_sets_the_phase_and_the_status_latch() {
        let mut core = AppCore::new();
        let run = core.start_diagnostics("id-web");
        core.apply(AppMsg::DiagnosticsError {
            id: "id-web".into(),
            run,
            message: "collect id-web: no such container".into(),
        });
        let st = core.diagnostics.get("id-web").expect("present");
        assert_eq!(
            st.phase,
            DiagnosticsPhase::Error("collect id-web: no such container".into())
        );
        assert_eq!(
            core.last_error.as_deref(),
            Some("collect id-web: no such container"),
            "§10: the status line latches it too"
        );
    }

    /// Review Focus #4.
    #[test]
    fn a_rerun_replaces_the_previous_text() {
        let mut core = AppCore::new();
        let r1 = core.start_diagnostics("id-web");
        core.apply(AppMsg::DiagnosticsAnalysis {
            id: "id-web".into(),
            run: r1,
            text: "first".into(),
        });
        let r2 = core.start_diagnostics("id-web");
        core.apply(AppMsg::DiagnosticsAnalysis {
            id: "id-web".into(),
            run: r2,
            text: "second".into(),
        });
        let st = core.diagnostics.get("id-web").expect("present");
        assert_eq!(st.analysis, "second", "replace, not append");
    }

    /// Review Focus #5.
    #[test]
    fn two_containers_run_independently() {
        let mut core = AppCore::new();
        let a = core.start_diagnostics("id-a");
        let b = core.start_diagnostics("id-b");
        core.apply(AppMsg::DiagnosticsAnalysis {
            id: "id-a".into(),
            run: a,
            text: "A".into(),
        });
        core.apply(AppMsg::DiagnosticsAnalysis {
            id: "id-b".into(),
            run: b,
            text: "B".into(),
        });
        assert_eq!(core.diagnostics["id-a"].analysis, "A");
        assert_eq!(core.diagnostics["id-b"].analysis, "B");
    }
}
