//! Desktop wrapper: inserts the Containers window at
//! startup and opens-or-focuses any of the six resource windows from
//! the Docker menu. Per-window state is a `Slot { open, id }` — the
//! open flag is cleared by `ResourceWindow::drop`: a closed window must
//! be re-openable, and `ctx.request_focus` on a stale id silently
//! no-ops (no public view-lookup exists), so we never guess.
use crate::bridge::Bridge;
use crate::core::{ActionReq, AppCore, AppMsg, ExplorerTarget, PendingKind, TaskReq};
use crate::views::{
    containers, details, events, images, logs, networks, shell, statistics, volumes,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use tv::{Context, Desktop, Event, Rect, View, ViewId, ViewState, delegate};

/// The six resource windows (Docker menu), indexing
/// `TvdockerDesktop::slots`.
#[derive(Clone, Copy)]
enum Win {
    Containers = 0,
    Images,
    Networks,
    Volumes,
    Events,
    Statistics,
}

/// One window's identity: the Drop-cleared open flag (reopen-vs-focus)
/// plus the last inserted id (focus target while it
/// is open).
struct Slot {
    open: Rc<RefCell<bool>>,
    id: Rc<RefCell<Option<ViewId>>>,
}

impl Slot {
    fn new() -> Self {
        Self {
            open: Rc::new(RefCell::new(false)),
            id: Rc::new(RefCell::new(None)),
        }
    }

    /// The id to focus when this slot already has a window ("opening an
    /// item already on screen focuses it instead of duplicating it"), or
    /// `None` once the window is gone (its `Drop` cleared the flag).
    fn open_id(&self) -> Option<ViewId> {
        if *self.open.borrow() {
            *self.id.borrow()
        } else {
            None
        }
    }

    /// The bookkeeping every opener does after `insert_and_focus`: the slot
    /// remembers the new id and flips the open flag (cleared by `Drop`).
    fn record(&self, vid: ViewId) {
        *self.id.borrow_mut() = Some(vid);
        *self.open.borrow_mut() = true;
    }
}

/// Every shell window opened for ONE container: one [`Slot`] per window
/// (each window's `Drop` clears its own flag), so "how many shells are
/// open" is a count, not a boolean. Closed slots are pruned on every
/// read; `opened` never shrinks — it numbers the labels so two live
/// windows can never share one (`webalpha (2)` stays unique).
struct ShellSet {
    slots: Vec<Slot>,
    /// Total windows ever opened for this container (label numbering).
    opened: usize,
}

impl ShellSet {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            opened: 0,
        }
    }

    /// Drop the closed windows, then how many are still open.
    fn live(&mut self) -> usize {
        self.slots.retain(|s| *s.open.borrow());
        self.slots.len()
    }

    /// The focus target for "show the current one": the first live slot.
    fn open_id(&mut self) -> Option<ViewId> {
        self.live();
        self.slots.first().and_then(|s| *s.id.borrow())
    }

    fn push(&mut self, slot: Slot) {
        self.live();
        self.slots.push(slot);
        self.opened += 1;
    }
}

/// Everything `finish_shell_prompt` needs after the yes/no box answers
/// (the `ShellSession` pattern).
struct ShellPrompt {
    id: String,
    name: String,
    image: String,
}

/// Everything `finish_shell_config` needs after the modal answers.
struct ShellSession {
    /// The answered form — seeded with the prefill, overwritten by the
    /// modal's record through `set_modal_data`.
    form: crate::views::dialogs::ShellForm,
    id: String,
    name: String,
    image: String,
}

/// One open run form (the shell-session pattern): the shared
/// values + the image the form runs (`repository:tag`) and its config
/// key (the base name).
struct RunSession {
    values: Rc<RefCell<crate::views::run_dialog::RunForm>>,
    image_ref: String,
    image_base: String,
}

pub struct TvdockerDesktop {
    desktop: Desktop,
    core: Rc<RefCell<AppCore>>,
    slots: [Slot; 6],
    /// Explorer windows keyed by container id / volume name.
    explorer_slots: HashMap<String, Slot>,
    /// Image-layer windows, keyed `"lay:<image id>"`.
    layers_slots: HashMap<String, Slot>,
    /// Next cascade step (resource windows "open cascaded over
    /// the desktop").
    cascade: Cell<usize>,
    filter: Option<String>,
    actions: tokio::sync::broadcast::Sender<ActionReq>,
    /// The task worker's sender — every Docker side
    /// effect the UI thread cannot perform is queued here.
    task_tx: tokio::sync::mpsc::UnboundedSender<TaskReq>,
    /// The answer of the last `request_message_box`/`request_exec_view`
    /// (routed by `set_modal_answer`, then acted on in the matching
    /// `*_CONFIRMED`/`*_SUBMIT` intercept.
    modal_answer: Option<tv::Command>,
    /// In-flight shell dialog state.
    shell_session: Option<ShellSession>,
    /// In-flight yes/no prompt ("a shell is already open").
    shell_prompt: Option<ShellPrompt>,
    /// In-flight run-form state (the shell-session pattern).
    run_session: Option<RunSession>,
    /// ×N slots keyed by container id.
    details_slots: HashMap<String, Slot>,
    log_slots: HashMap<String, Slot>,
    /// Diagnostics windows keyed by container id.
    diagnostics_slots: HashMap<String, Slot>,
    /// Every shell window per container id (one [`Slot`] each).
    shell_slots: HashMap<String, ShellSet>,
    /// The single Settings window (settings window number 0, so
    /// there is exactly one of it).
    settings_slot: Option<Slot>,
}

/// Which ×N Explorer family a window belongs to: its slot-map key prefix,
/// how the queued id becomes a target, and its label (image-explorer §4).
enum ExplorerKind {
    Container,
    Volume,
    Image,
}

impl ExplorerKind {
    fn slot_prefix(&self) -> &'static str {
        match self {
            ExplorerKind::Container => "",
            ExplorerKind::Volume => "vol:",
            ExplorerKind::Image => "img:",
        }
    }

    fn target(&self, id: &str) -> ExplorerTarget {
        match self {
            ExplorerKind::Container => ExplorerTarget::Container(id.to_string()),
            ExplorerKind::Volume => ExplorerTarget::Volume(id.to_string()),
            ExplorerKind::Image => ExplorerTarget::Image(id.to_string()),
        }
    }

    /// Containers show the container name, volumes their name, images their
    /// `repository:tag` (falling back to a truncated id).
    fn label(&self, core: &AppCore, id: &str) -> String {
        match self {
            ExplorerKind::Container => {
                let name = core
                    .containers
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                if name.is_empty() {
                    id.to_string()
                } else {
                    name
                }
            }
            ExplorerKind::Volume => id.to_string(),
            ExplorerKind::Image => core
                .images
                .iter()
                .find(|i| i.id == id)
                .map(|i| format!("{}:{}", i.repository, i.tag))
                .unwrap_or_else(|| id.chars().take(12).collect()),
        }
    }
}

impl TvdockerDesktop {
    fn open_or_focus(&mut self, kind: Win, ctx: &mut Context) {
        // The same command also requests an immediate poll;
        // the broadcast wakes EVERY poller. Nobody
        // listening ⇒ `send` errs and the request is dropped — the
        // interval ticks cover it.
        let _ = self.actions.send(ActionReq::PollNow);
        let slot = &self.slots[kind as usize];
        if let Some(id) = slot.open_id() {
            ctx.request_focus(id); // already open → focus
            return;
        }
        let step = self.cascade.get();
        self.cascade.set(step.wrapping_add(1));
        let bounds = window_bounds_for(&self.desktop, step);
        let open = Rc::clone(&slot.open);
        let win = match kind {
            Win::Containers => {
                containers::build(Rc::clone(&self.core), bounds, self.filter.as_deref(), open)
            }
            Win::Images => images::build(Rc::clone(&self.core), bounds, open),
            Win::Networks => networks::build(Rc::clone(&self.core), bounds, open),
            Win::Volumes => volumes::build(Rc::clone(&self.core), bounds, open),
            Win::Events => events::build(Rc::clone(&self.core), bounds, open),
            Win::Statistics => statistics::build(Rc::clone(&self.core), bounds, open),
        };
        let id = self.desktop.insert_and_focus(Box::new(win), ctx);
        slot.record(id);
    }

    /// `(name, image)` of a container id (empty when unknown — the
    /// windowers then bail on an empty pending list).
    fn container_meta(&self, id: &str) -> (String, String) {
        self.core
            .borrow()
            .containers
            .iter()
            .find(|c| c.id == id)
            .map(|c| (c.name.clone(), c.image.clone()))
            .unwrap_or_default()
    }

    /// The next cascade bounds (geometry, reused by the
    /// 3B windows).
    fn next_bounds(&mut self) -> Rect {
        let step = self.cascade.get();
        self.cascade.set(step.wrapping_add(1));
        window_bounds_for(&self.desktop, step)
    }

    /// Details (amended): one window PER container;
    /// re-inspect per Enter, focusing an already-open window.
    fn open_details(&mut self, ctx: &mut Context) {
        let Some(target) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        let (name, _) = self.container_meta(&target);
        let focused = self
            .details_slots
            .entry(target.clone())
            .or_insert_with(Slot::new)
            .open_id();
        self.core.borrow_mut().details.remove(&target); // loading state
        let _ = self.task_tx.send(TaskReq::Inspect { id: target.clone() });
        if let Some(vid) = focused {
            ctx.request_focus(vid);
            return;
        }
        let display = if name.is_empty() {
            target.clone()
        } else {
            name
        };
        let bounds = self.next_bounds();
        let open = Rc::clone(
            &self
                .details_slots
                .entry(target.clone())
                .or_insert_with(Slot::new)
                .open,
        );
        let win = details::build(Rc::clone(&self.core), bounds, &target, &display, open);
        let vid = self.desktop.insert_and_focus(Box::new(win), ctx);
        let slot = self.details_slots.entry(target).or_insert_with(Slot::new);
        slot.record(vid);
    }

    /// Logs: fresh session + worker + ×N window.
    fn open_logs(&mut self, ctx: &mut Context) {
        let Some(target) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        let (name, _) = self.container_meta(&target);
        let focused = self
            .log_slots
            .entry(target.clone())
            .or_insert_with(Slot::new)
            .open_id();
        if let Some(vid) = focused {
            ctx.request_focus(vid);
            return;
        }
        self.core.borrow_mut().set_logs_session(&target);
        let stop = self
            .core
            .borrow()
            .logs
            .get(&target)
            .map(|b| b.stop.clone())
            .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let _ = self.task_tx.send(TaskReq::StartLogs {
            id: target.clone(),
            stop,
        });
        let display = if name.is_empty() {
            target.clone()
        } else {
            name
        };
        let bounds = self.next_bounds();
        let open = Rc::clone(
            &self
                .log_slots
                .entry(target.clone())
                .or_insert_with(Slot::new)
                .open,
        );
        let win = logs::build(Rc::clone(&self.core), bounds, &target, &display, open);
        let vid = self.desktop.insert_and_focus(Box::new(win), ctx);
        let slot = self.log_slots.entry(target).or_insert_with(Slot::new);
        slot.record(vid);
    }

    /// Diagnostics: one window per container; the
    /// `[llm]` config is re-read each run; re-diagnosing an open window
    /// restarts the run in place. A failure rides §10 (window phase +
    /// status latch) — never a modal.
    fn open_diagnostics(&mut self, ctx: &mut Context) {
        let Some(target) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        let (name, _) = self.container_meta(&target);
        // §11: re-read `[llm]` for EVERY run — a cached config would defeat it.
        let config = crate::domain::config::TvdockerConfig::load()
            .0
            .llm
            .unwrap_or_default();
        // Start the run BEFORE the open/focus check so a re-run restarts.
        let run = self.core.borrow_mut().start_diagnostics(&target);
        let _ = self.task_tx.send(TaskReq::StartDiagnostics {
            id: target.clone(),
            run,
            config,
        });
        // Focus an already-open window (its rev-diff picks up the new run).
        let focused = self
            .diagnostics_slots
            .entry(target.clone())
            .or_insert_with(Slot::new)
            .open_id();
        if let Some(vid) = focused {
            ctx.request_focus(vid);
            return;
        }
        let display = if name.is_empty() {
            target.clone()
        } else {
            name
        };
        let bounds = self.next_bounds();
        let open = Rc::clone(
            &self
                .diagnostics_slots
                .entry(target.clone())
                .or_insert_with(Slot::new)
                .open,
        );
        let mut win = crate::views::diagnostics::build(
            Rc::clone(&self.core),
            bounds,
            &target,
            &display,
            open,
        );
        win.request_initial_focus(ctx); // analysis pane, not the settle-pass pick
        let vid = self.desktop.insert_and_focus(Box::new(win), ctx);
        let slot = self
            .diagnostics_slots
            .entry(target)
            .or_insert_with(Slot::new);
        slot.record(vid);
    }

    /// Image jump: open/focus the Images window, then queue the row-focus
    /// request for it and broadcast (the desktop holds only a `ViewId` —
    /// there is no view-lookup to reach the window's table directly).
    /// A pruned image (no row) still opens the window; the cursor just
    /// stays put.
    fn open_images_for_container(&mut self, ctx: &mut Context) {
        let Some(container_id) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        // Focus requests are one-shot: clear any stale one BEFORE opening,
        // so a menu-open can never jump to a leftover row.
        self.core.borrow_mut().take_focus_request();
        self.open_or_focus(Win::Images, ctx);
        let row =
            crate::views::images::image_row_id_for_container(&self.core.borrow(), &container_id);
        if let Some(row_id) = row {
            self.core.borrow_mut().set_focus_request(row_id);
            ctx.broadcast(crate::app::CMD_FOCUS_REQUEST, None);
        }
    }

    /// Open an Explorer on the queued id — containers, volumes and images
    /// share this one path (image-explorer §4). The slot is keyed
    /// `"{prefix}{id}"`, so a second invoke focuses the existing window.
    fn open_explorer(&mut self, kind: ExplorerKind, ctx: &mut Context) {
        let Some(target_id) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        let slot_key = format!("{}{target_id}", kind.slot_prefix());
        let focused = self
            .explorer_slots
            .entry(slot_key.clone())
            .or_insert_with(Slot::new)
            .open_id();
        if let Some(vid) = focused {
            ctx.request_focus(vid);
            return;
        }
        let label = kind.label(&self.core.borrow(), &target_id);
        let bounds = self.next_bounds();
        let open = Rc::clone(
            &self
                .explorer_slots
                .entry(slot_key.clone())
                .or_insert_with(Slot::new)
                .open,
        );
        let mut win = crate::views::explorer::build(
            Rc::clone(&self.core),
            bounds,
            slot_key.clone(),
            kind.target(&target_id),
            label,
            open,
            self.task_tx.clone(),
        );
        win.request_initial_focus(ctx); // host panel, not the settle-pass pick
        let vid = self.desktop.insert_and_focus(Box::new(win), ctx);
        let slot = self
            .explorer_slots
            .entry(slot_key)
            .or_insert_with(Slot::new);
        slot.record(vid);
    }

    /// The image-layer window: keyed `lay:<image id>`, so a second invoke
    /// focuses the existing window (the Explorer's slot discipline).
    fn open_layers(&mut self, ctx: &mut Context) {
        let Some(image_id) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        let slot_key = format!("lay:{image_id}");
        if let Some(vid) = self
            .layers_slots
            .entry(slot_key.clone())
            .or_insert_with(Slot::new)
            .open_id()
        {
            ctx.request_focus(vid);
            return;
        }
        let bounds = self.next_bounds();
        let open = Rc::clone(
            &self
                .layers_slots
                .entry(slot_key.clone())
                .or_insert_with(Slot::new)
                .open,
        );
        let label = self
            .core
            .borrow()
            .images
            .iter()
            .find(|i| i.id == image_id)
            .map(|i| {
                if i.repository.is_empty() {
                    format!("{}…", &i.id[..12.min(i.id.len())])
                } else {
                    format!("{}:{}", i.repository, i.tag)
                }
            })
            .unwrap_or_else(|| image_id.clone());
        let mut win = crate::views::layers::build(
            Rc::clone(&self.core),
            bounds,
            image_id,
            label,
            open,
            self.task_tx.clone(),
        );
        win.request_initial_focus(ctx);
        let vid = self.desktop.insert_and_focus(Box::new(win), ctx);
        let slot = self.layers_slots.entry(slot_key).or_insert_with(Slot::new);
        slot.record(vid);
    }

    /// Settings (settings): open the one window, or focus it when it
    /// is already open (no second buffer).
    fn open_settings(&mut self, ctx: &mut Context) {
        let focused = self.settings_slot.as_ref().and_then(Slot::open_id);
        if let Some(vid) = focused {
            ctx.request_focus(vid);
            return;
        }
        let bounds = self.next_bounds();
        let open = Rc::clone(&self.settings_slot.get_or_insert_with(Slot::new).open);
        let win = crate::views::settings::build(bounds, Rc::clone(&self.core), open);
        let vid = self.desktop.insert_and_focus(Box::new(win), ctx);
        let slot = self.settings_slot.get_or_insert_with(Slot::new);
        slot.record(vid);
    }

    /// F4: dialog; but with EXACTLY one shell already open for the
    /// container, a yes/no prompt first (No ⇒ show the current one, no
    /// new exec). Zero or several open ⇒ straight to the dialog.
    fn open_shell_config(&mut self, ctx: &mut Context) {
        let Some(target) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        let (name, image) = self.container_meta(&target);
        let live = self
            .shell_slots
            .entry(target.clone())
            .or_insert_with(ShellSet::new)
            .live();
        if live == 1 {
            let Some(requester) = self.state().id() else {
                return;
            };
            self.shell_prompt = Some(ShellPrompt {
                id: target,
                name: name.clone(),
                image,
            });
            ctx.request_message_box(
                format!(
                    "A shell is already open for '{name}'. Open a new one? \
                     (No = show the current one)"
                ),
                tv::MessageBoxKind::Confirmation,
                tv::MessageBoxButtons::yes_no(),
                Some(requester),
                Some(crate::app::CMD_SHELL_PROMPT),
            );
            return;
        }
        self.open_shell_dialog(target, name, image, ctx);
    }

    /// The yes/no answer: Yes ⇒ the normal config dialog; anything else
    /// (No/Esc/CANCEL) ⇒ focus the shell that is already open — no exec.
    fn finish_shell_prompt(&mut self, ctx: &mut Context) {
        let answer = self.modal_answer.take();
        let Some(prompt) = self.shell_prompt.take() else {
            return;
        };
        if answer == Some(tv::Command::YES) {
            self.open_shell_dialog(prompt.id, prompt.name, prompt.image, ctx);
            return;
        }
        let set = self
            .shell_slots
            .entry(prompt.id)
            .or_insert_with(ShellSet::new);
        if let Some(vid) = set.open_id() {
            ctx.request_focus(vid);
        }
    }

    /// The config dialog for `(id, name, image)` — every F4 path ends
    /// here (directly, or through the prompt's Yes).
    fn open_shell_dialog(&mut self, id: String, name: String, image: String, ctx: &mut Context) {
        let form = crate::views::dialogs::ShellForm::prefill(&name, &image);
        self.shell_session = Some(ShellSession {
            form: form.clone(),
            id: id.clone(),
            name,
            image,
        });
        let Some(requester) = self.state().id() else {
            return;
        };
        let size = ctx.owner_size();
        let bounds = Rect::new(
            (size.x - 60) / 2,
            (size.y - 10) / 2,
            (size.x - 60) / 2 + 60,
            (size.y - 10) / 2 + 10,
        );
        let dialog = crate::views::dialogs::ShellConfigDialog::new(bounds, &form, &id);
        ctx.request_exec_view(
            Box::new(dialog),
            requester,
            Some(crate::app::CMD_SHELL_SUBMIT),
        );
    }

    /// After the dialog answers OK: persist (only-on-
    /// change), create the channels, start the exec, open the window.
    fn finish_shell_config(&mut self, ctx: &mut Context) {
        if self.modal_answer.take() != Some(tv::Command::OK) {
            self.shell_session = None;
            return;
        }
        let Some(sess) = self.shell_session.take() else {
            return;
        };
        let form = sess.form;
        if let Err(e) = form.save_if_changed(&sess.name, &sess.image) {
            // Never a modal (§10) — the status line carries it.
            self.core.borrow_mut().last_error = Some(format!("config save: {e}"));
        }
        let container_id = sess.id;
        let (out_tx, out_rx) = tokio::sync::mpsc::channel(256);
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (resize_tx, resize_rx) = tokio::sync::watch::channel((80u16, 24u16));
        let _ = self.task_tx.send(TaskReq::StartShell {
            id: container_id.clone(),
            shell: form.shell,
            user: form.user,
            workdir: form.workdir,
            out_tx,
            input_rx,
            resize_rx,
        });
        // Every window gets ITS OWN slot (the Drop flag it clears when it
        // closes). The label counts this container's windows ever opened:
        // the first keeps the plain name, then `name (2)`, `name (3)`, …
        // and the counter never rewinds, so two live windows stay unique.
        let label = {
            let set = self
                .shell_slots
                .entry(container_id.clone())
                .or_insert_with(ShellSet::new);
            if set.opened == 0 {
                sess.name.clone()
            } else {
                format!("{} ({})", sess.name, set.opened + 1)
            }
        };
        let slot = Slot::new();
        let open = Rc::clone(&slot.open);
        let bounds = self.next_bounds();
        let (win, _state) = shell::build(bounds, &label, input_tx, resize_tx, out_rx, open);
        let vid = self.desktop.insert_and_focus(Box::new(win), ctx);
        slot.record(vid);
        self.shell_slots
            .entry(container_id)
            .or_insert_with(ShellSet::new)
            .push(slot);
    }

    /// Run…: the form for the cursor image (upstream parity:
    /// one form per invocation — no slot map, the form is modal).
    fn open_run_form(&mut self, ctx: &mut Context) {
        let Some(target) = self.core.borrow_mut().take_pending().into_iter().next() else {
            return;
        };
        let image_ref = self.image_ref(&target);
        let image_base = crate::views::dialogs::image_base_name(&image_ref).to_string();
        let values = Rc::new(RefCell::new(crate::views::run_dialog::RunForm::prefill(
            &image_base,
        )));
        self.run_session = Some(RunSession {
            values: Rc::clone(&values),
            image_ref,
            image_base,
        });
        let Some(requester) = self.state().id() else {
            return;
        };
        let size = ctx.owner_size();
        let bounds = Rect::new(
            (size.x - 68) / 2,
            (size.y - 19) / 2,
            (size.x - 68) / 2 + 68,
            (size.y - 19) / 2 + 19,
        );
        let dialog = crate::views::run_dialog::RunDialog::new(bounds, values);
        ctx.request_exec_view(
            Box::new(dialog),
            requester,
            Some(crate::app::CMD_RUN_SUBMIT),
        );
    }

    /// OK ⇒ persist shell/user (save-if-changed) + queue the run; any
    /// other answer just drops the session.
    fn finish_run(&mut self) {
        if self.modal_answer.take() != Some(tv::Command::OK) {
            self.run_session = None;
            return;
        }
        let Some(sess) = self.run_session.take() else {
            return;
        };
        let form = sess.values.borrow().clone();
        if let Err(e) = form.save_if_changed(&sess.image_base) {
            // Never a modal (§10) — the status line carries it.
            self.core.borrow_mut().last_error = Some(format!("config save: {e}"));
        }
        let _ = self.task_tx.send(TaskReq::RunContainer {
            opts: Box::new(form.to_opts(&sess.image_ref)),
        });
    }

    /// `repository:tag` for one image row id (falls back to a short id).
    fn image_ref(&self, id: &str) -> String {
        self.core
            .borrow()
            .images
            .iter()
            .find(|i| i.id == id)
            .map(|i| format!("{}:{}", i.repository, i.tag))
            .unwrap_or_else(|| id.chars().take(12).collect())
    }

    /// The confirm text for the pending kind (one flow:
    /// text names the resource).
    fn confirm_text(&self, kind: PendingKind, targets: &[String]) -> String {
        let plural = |one: &str, many: &str| {
            if targets.len() == 1 {
                format!("{one} '{}'?", self.resource_label(kind, &targets[0]))
            } else {
                format!("{many} {}?", targets.len())
            }
        };
        match kind {
            PendingKind::Containers => plural("Delete container", "Delete containers"),
            PendingKind::Images => plural("Remove image", "Remove images"),
            PendingKind::Networks => plural("Delete network", "Delete networks"),
            PendingKind::Volumes => plural("Delete volume", "Delete volumes"),
            PendingKind::DanglingImages => "Remove all dangling images?".to_string(),
            PendingKind::PruneImages => "Prune all unused images?".to_string(),
        }
    }

    /// The display name for one target of `kind` (falls back to a short
    /// id when the row vanished between click and confirm).
    fn resource_label(&self, kind: PendingKind, id: &str) -> String {
        let core = self.core.borrow();
        match kind {
            PendingKind::Containers => core
                .containers
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| id.chars().take(12).collect()),
            PendingKind::Images => core
                .images
                .iter()
                .find(|i| i.id == id)
                .map(|i| format!("{}:{}", i.repository, i.tag))
                .unwrap_or_else(|| id.chars().take(12).collect()),
            PendingKind::Networks => core
                .networks
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| id.chars().take(12).collect()),
            PendingKind::Volumes | PendingKind::DanglingImages | PendingKind::PruneImages => {
                id.to_string()
            }
        }
    }

    fn confirm_delete(&mut self, ctx: &mut Context) {
        let kind = self.core.borrow().pending_kind;
        let targets = self.core.borrow().pending_targets.clone();
        // The batch sweeps carry no targets — they still confirm.
        if targets.is_empty()
            && !matches!(kind, PendingKind::DanglingImages | PendingKind::PruneImages)
        {
            return;
        }
        let text = self.confirm_text(kind, &targets);
        let Some(requester) = self.state().id() else {
            return;
        };
        ctx.request_message_box(
            text,
            tv::MessageBoxKind::Confirmation,
            tv::MessageBoxButtons::yes_no(),
            Some(requester),
            Some(crate::app::CMD_DELETE_CONFIRMED),
        );
    }

    /// Yes ⇒ one `TaskReq` per target, mapped by kind; No/Cancel ⇒ drop
    /// the targets AND reset the kind (Review Focus #4).
    fn finish_delete(&mut self) {
        let kind = self.core.borrow_mut().take_kind();
        let targets = self.core.borrow_mut().take_pending();
        if self.modal_answer.take() != Some(tv::Command::YES) {
            return;
        }
        for id in targets {
            let req = match kind {
                PendingKind::Containers => TaskReq::RemoveContainer { id },
                PendingKind::Images => TaskReq::RemoveImage { id },
                PendingKind::Networks => TaskReq::RemoveNetwork { id },
                PendingKind::Volumes => TaskReq::RemoveVolume { name: id },
                // Batch sweeps carry no targets — sent once, below.
                PendingKind::DanglingImages | PendingKind::PruneImages => continue,
            };
            let _ = self.task_tx.send(req);
        }
        match kind {
            PendingKind::DanglingImages => {
                let _ = self.task_tx.send(TaskReq::RemoveDanglingImages);
            }
            PendingKind::PruneImages => {
                let _ = self.task_tx.send(TaskReq::PruneImages);
            }
            _ => {}
        }
    }

    /// Start/Stop: pending targets filtered by CURRENT state (bollard
    /// errors on no-ops).
    fn set_state_pending(&mut self, start: bool) {
        let targets = self.core.borrow_mut().take_pending();
        let wanted: Vec<String> = {
            let core = self.core.borrow();
            targets
                .into_iter()
                .filter(|id| {
                    core.containers
                        .iter()
                        .find(|c| c.id == *id)
                        .is_some_and(|c| (c.state == "running") != start)
                })
                .collect()
        };
        for id in wanted {
            let _ = self.task_tx.send(TaskReq::SetContainerState { id, start });
        }
    }
}

/// Cascade geometry ("classic cascade"): origin
/// +2/+1 per open, wrapping every FIFTH window, sized DOWN to stay
/// inside the desktop — so step 0 is the fixed menu-open rect while
/// later steps stagger diagonally (80×25 fits all
/// five; the size clamp is what keeps them on-screen).
fn window_bounds_for(desktop: &Desktop, step: usize) -> Rect {
    let size = desktop.state().get_bounds();
    let i = (step % 5) as i32;
    let x = size.a.x + 2 + 2 * i;
    let y = size.a.y + 1 + i;
    let w = (size.b.x - x).clamp(0, 76);
    let h = (size.b.y - y).clamp(0, 22);
    Rect::new(x, y, x + w, y + h)
}

#[delegate(to = desktop)]
impl View for TvdockerDesktop {
    fn state(&self) -> &ViewState {
        self.desktop.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.desktop.state_mut()
    }
    fn draw(&mut self, ctx: &mut tv::DrawCtx) {
        self.desktop.draw(ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // Intercept BEFORE delegation: the command may come from the menu
        // or the status pre-route; consume it so the inner desktop's
        // children never see an unknown command.
        if let Event::Command(cmd) = ev {
            // The six open-or-focus commands (unchanged behavior).
            let win = if *cmd == crate::app::CMD_OPEN_CONTAINERS {
                Some(Win::Containers)
            } else if *cmd == crate::app::CMD_OPEN_IMAGES {
                Some(Win::Images)
            } else if *cmd == crate::app::CMD_OPEN_NETWORKS {
                Some(Win::Networks)
            } else if *cmd == crate::app::CMD_OPEN_VOLUMES {
                Some(Win::Volumes)
            } else if *cmd == crate::app::CMD_OPEN_EVENTS {
                Some(Win::Events)
            } else if *cmd == crate::app::CMD_OPEN_STATISTICS {
                Some(Win::Statistics)
            } else {
                None
            };
            if let Some(kind) = win {
                self.open_or_focus(kind, ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_OPEN_DETAILS {
                self.open_details(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_OPEN_LOGS {
                self.open_logs(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_DIAGNOSTICS {
                self.open_diagnostics(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_EXPLORER {
                self.open_explorer(ExplorerKind::Container, ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_VOLUME_EXPLORER {
                self.open_explorer(ExplorerKind::Volume, ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_IMAGE_EXPLORER {
                self.open_explorer(ExplorerKind::Image, ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_SHOW_IMAGE {
                self.open_images_for_container(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_LAYERS {
                self.open_layers(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_SETTINGS {
                self.open_settings(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_SHELL_CONFIG {
                self.open_shell_config(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_SHELL_SUBMIT {
                self.finish_shell_config(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_SHELL_PROMPT {
                self.finish_shell_prompt(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_RUN {
                self.open_run_form(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_RUN_SUBMIT {
                self.finish_run();
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_DELETE {
                self.confirm_delete(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_DELETE_CONFIRMED {
                self.finish_delete();
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_START {
                self.set_state_pending(true);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_STOP {
                self.set_state_pending(false);
                ev.clear();
                return;
            }
        }
        self.desktop.handle_event(ev, ctx);
    }

    /// Answer route for BOTH modal kinds: the pump
    /// routes the box/dialog's close command here, then the posted
    /// `*_SUBMIT`/`*_CONFIRMED` command performs the action.
    fn set_modal_answer(&mut self, cmd: tv::Command) {
        self.modal_answer = Some(cmd);
    }

    /// The shell config dialog's record: the three field texts, in order
    /// (Shell, User, Workdir). The pump delivers this BEFORE the close
    /// command, so `finish_shell_config` sees the answered form.
    fn set_modal_data(&mut self, data: tv::FieldValue) {
        let tv::FieldValue::List(items) = data else {
            return;
        };
        let Some(sess) = self.shell_session.as_mut() else {
            return;
        };
        let mut texts = items.into_iter().filter_map(|v| match v {
            tv::FieldValue::Text(t) => Some(t),
            _ => None,
        });
        if let Some(shell) = texts.next() {
            sess.form.shell = shell;
        }
        if let Some(user) = texts.next() {
            sess.form.user = user;
        }
        if let Some(workdir) = texts.next() {
            sess.form.workdir = workdir;
        }
    }

    /// Enter the desktop at its edge — required for the hierarchical Tab
    /// wrap (`View::focus_to_edge` base returns "focused me" without
    /// descending; `#[delegate]` does not cover it, observed: Tab died
    /// at the wrapper instead of cycling back to the filter).
    fn focus_to_edge(&mut self, backward: bool, ctx: &mut tv::Context) -> bool {
        self.desktop.focus_to_edge(backward, ctx)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// The startup desktop (Containers opens at startup). Also
/// inserts the [`Bridge`] as a zero-area child — it drains `app_rx` into
/// the shared core on its 50 ms timer (pty_probe precedent).
pub fn containers_desktop(
    r: Rect,
    core: Rc<RefCell<AppCore>>,
    filter: Option<&str>,
    app_rx: tokio::sync::mpsc::Receiver<AppMsg>,
    actions: tokio::sync::broadcast::Sender<ActionReq>,
    task_tx: tokio::sync::mpsc::UnboundedSender<crate::core::TaskReq>,
) -> Option<Box<dyn View>> {
    let mut r = r;
    r.a.y += 1;
    r.b.y -= 1;
    let inner = Desktop::new(r, |br| Some(Desktop::init_background(br)));
    let mut dw = TvdockerDesktop {
        desktop: inner,
        core: Rc::clone(&core),
        slots: std::array::from_fn(|_| Slot::new()),
        cascade: Cell::new(0),
        explorer_slots: HashMap::new(),
        layers_slots: HashMap::new(),
        filter: filter.map(str::to_string),
        actions,
        task_tx,
        modal_answer: None,
        shell_session: None,
        shell_prompt: None,
        run_session: None,
        details_slots: HashMap::new(),
        log_slots: HashMap::new(),
        diagnostics_slots: HashMap::new(),
        shell_slots: HashMap::new(),
        settings_slot: None,
    };
    // Containers opens at startup ; its slot records the id
    // and flips the Drop flag ON (slots start false).
    let win = containers::build(
        Rc::clone(&core),
        Rect::new(2, 1, 78, 23),
        filter,
        Rc::clone(&dw.slots[0].open),
    );
    let id = dw.desktop.insert_view(Box::new(win));
    *dw.slots[0].id.borrow_mut() = Some(id);
    *dw.slots[0].open.borrow_mut() = true;
    dw.desktop
        .insert_view(Box::new(Bridge::new(core, app_rx, AppCore::apply)));
    Some(Box::new(dw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop_at(rect: Rect) -> Desktop {
        Desktop::new(rect, |br| Some(Desktop::init_background(br)))
    }

    #[test]
    fn cascade_offsets_wrap_every_fifth_window_and_stay_inside() {
        let d = desktop_at(Rect::new(0, 1, 80, 24)); // the real 80×25 area
        assert_eq!(
            window_bounds_for(&d, 0),
            Rect::new(2, 2, 78, 24),
            "step 0 = the fixed menu-open rect"
        );
        assert_eq!(
            window_bounds_for(&d, 1),
            Rect::new(4, 3, 80, 24),
            "+2/+1 origin, sized to fit the desktop"
        );
        assert_eq!(
            window_bounds_for(&d, 4),
            Rect::new(10, 6, 80, 24),
            "the deepest step still fits 80 columns"
        );
        assert_eq!(
            window_bounds_for(&d, 5),
            window_bounds_for(&d, 0),
            "wraps every fifth window"
        );
    }
}
