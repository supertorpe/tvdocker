//! The Image Run form (modal; §7 "Run… modal form (all fields
//! incl. advanced)"): one tall window, one Tab chain,
//! validation on submit (upstream rules), values handed back through a
//! shared `Rc<RefCell<RunForm>>` (shell-config pattern).

use crate::domain::docker::images::ContainerOpts;
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key};
use tv::{
    CheckBoxes, Context, DrawCtx, FieldValue, InputLine, Rect, Role, StaticText, View, ViewState,
    Window, delegate,
};

/// One collected run (vendored tvdocker `ImageRunState`; empty string =
/// "container default"). Multi-value fields (env/ports/volumes/labels)
/// are `;`-separated on the single input line and become `\n`-joined in
/// `to_opts` — the domain splitter stays vendored-verbatim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunForm {
    pub command: String,
    pub shell: String,
    pub user: String,
    pub workdir: String,
    pub env: String,
    pub ports: String,
    pub volumes: String,
    pub name: String,
    pub restart: String,
    pub memory: String,
    pub cpu: String,
    pub network: String,
    pub labels: String,
    pub autoremove: bool,
    pub privileged: bool,
}

impl RunForm {
    /// Prefill from the config chain (upstream saves shell/user per
    /// IMAGE on submit): `images[image_base]` → `latest_shell` → empty.
    pub fn prefill(image_base: &str) -> RunForm {
        let (config, _err) = crate::domain::config::TvdockerConfig::load();
        Self::prefill_from(&config, image_base)
    }

    /// `prefill` against an explicit config (the pure core — the
    /// no-write decision compares against exactly this).
    pub fn prefill_from(
        config: &crate::domain::config::TvdockerConfig,
        image_base: &str,
    ) -> RunForm {
        let per_image = config.images.get(image_base);
        RunForm {
            shell: per_image
                .and_then(|c| c.shell.clone())
                .or_else(|| config.latest_shell.clone())
                .unwrap_or_default(),
            user: per_image.and_then(|c| c.user.clone()).unwrap_or_default(),
            ..Default::default()
        }
    }

    /// The domain options: `;`-separated UI values become `\n`-joined
    /// (the vendored `create_container` splits on `\n`).
    pub fn to_opts(&self, image_ref: &str) -> ContainerOpts {
        let join = |s: &str| {
            s.split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        };
        ContainerOpts {
            image: image_ref.to_string(),
            cmd: self.command.clone(),
            shell: self.shell.clone(),
            user: self.user.clone(),
            workdir: self.workdir.clone(),
            env_vars: join(&self.env),
            port_mapping: join(&self.ports),
            volumes: join(&self.volumes),
            name: self.name.clone(),
            autoremove: self.autoremove,
            restart_policy: self.restart.clone(),
            memory_limit: self.memory.clone(),
            cpu_limit: self.cpu.clone(),
            network: self.network.clone(),
            labels: join(&self.labels),
            privileged: self.privileged,
        }
    }

    /// Persist shell/user under `images[image_base]` (upstream parity) —
    /// an UNEDITED submit never touches the file, on ANY config (decision
    /// #20; the reviewer's finding 1: the old path-gated virgin check
    /// still wrote on a non-virgin config). Returns whether it wrote.
    /// `Err(String)` carries the user-facing reason (see `validate` above).
    pub fn save_if_changed(&self, image_base: &str) -> Result<bool, String> {
        let (mut config, err) = crate::domain::config::TvdockerConfig::load();
        if let Some(e) = err {
            return Err(e); // a broken file must be fixed by hand first
        }
        let Some(row) = self.persist_row(&config, image_base) else {
            return Ok(false);
        };
        config.images.insert(image_base.to_string(), row);
        config.save()?;
        Ok(true)
    }

    /// The row a submit should persist, or `None` when it must not touch
    /// the file at all. Pure over the loaded config so the no-write
    /// decision is unit-testable without touching any file (the
    /// hermetic integration suite shares one per-process config path).
    fn persist_row(
        &self,
        config: &crate::domain::config::TvdockerConfig,
        image_base: &str,
    ) -> Option<crate::domain::config::ContainerShellConfig> {
        // Unedited ⇒ nothing to persist — compare against the very
        // prefill this config yields, whatever the file holds.
        if *self == Self::prefill_from(config, image_base) {
            return None;
        }
        // The run form never collects workdir — PRESERVE whatever the
        // F4 shell dialog stored for this image (reviewer finding 1:
        // hard-coding `None` silently erased it on every submit).
        Some(crate::domain::config::ContainerShellConfig {
            shell: nonempty(self.shell.clone()),
            user: nonempty(self.user.clone()),
            workdir: config
                .images
                .get(image_base)
                .and_then(|c| c.workdir.clone()),
        })
    }
}

fn nonempty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// Upstream's submit validation (vendored `reducers/image.rs:159-182`,
/// adapted to the `;` separator): ports are `HOST:CONTAINER` or a bare
/// numeric container port; volumes must carry a `:`.
/// `Err(String)` on purpose: the string IS the message shown to the
/// user (`if let Err(msg)` at the call site, and the tests assert on its
/// text) — it is a message, not an error chain, so `anyhow` would be a
/// downgrade here rather than the crate-wide default (this is why
/// `domain/config.rs` reads `String`-returning too).
pub fn validate(form: &RunForm) -> Result<(), String> {
    for line in form.ports.split(';') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains(',') {
            // Review Focus #1: docker-CLI muscle memory types commas —
            // name the separator the form actually wants.
            return Err(format!(
                "Invalid port '{line}': separate multiple entries with ';', not ','"
            ));
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() > 3 {
            return Err(format!(
                "Invalid port '{line}': use HOST:CONTAINER or CONTAINER (separate multiple with ';')"
            ));
        }
        let Some(container_port) = parts.last() else {
            continue;
        };
        if container_port.trim().parse::<u16>().is_err() {
            return Err(format!(
                "Invalid container port '{container_port}' in '{line}': must be a number"
            ));
        }
    }
    for line in form.volumes.split(';') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains(',') {
            return Err(format!(
                "Invalid volume '{line}': separate multiple entries with ';', not ','"
            ));
        }
        if !line.contains(':') {
            return Err(format!(
                "Invalid volume '{line}': must be HOST:CONTAINER[:ro|:rw] (separate multiple with ';')"
            ));
        }
    }
    // Reviewer finding 9: `create_container` silently DROPS an
    // unparseable limit (`if let Ok`) — the container would run
    // unlimited with no error anywhere. Refuse at the form instead.
    let memory = form.memory.trim();
    if !memory.is_empty() && crate::domain::docker::images::parse_memory(memory).is_err() {
        return Err(format!(
            "Invalid memory '{memory}': use 512m, 1g, 512mb, or plain bytes"
        ));
    }
    let cpu = form.cpu.trim();
    if !cpu.is_empty() && cpu.parse::<f64>().is_err() {
        return Err(format!(
            "Invalid cpu '{cpu}': use a number of cores, e.g. 0.5"
        ));
    }
    Ok(())
}

/// The field labels, in TAB order (visual order — the window walks
/// insertion order, the shell-config lesson from bug 1).
const LABELS: [&str; 13] = [
    "Command", "Shell", "User", "Workdir", "Env", "Ports", "Volumes", "Name", "Restart", "Memory",
    "CPU", "Network", "Labels",
];

/// The form window: 13 `InputLine`s (labels left, inputs right), a
/// `CheckBoxes` group (autoremove/privileged), and a hint/error row.
/// Enter validates then ends the modal with `Command::OK`; Esc cancels.
pub struct RunDialog {
    win: Window,
    values: Rc<RefCell<RunForm>>,
    field_ids: Vec<tv::ViewId>,
    checkboxes: tv::ViewId,
    error: Rc<RefCell<String>>,
}

impl RunDialog {
    /// `values` is prefilled by the caller (the desktop) and read by it
    /// after `Command::OK`.
    pub fn new(bounds: Rect, values: Rc<RefCell<RunForm>>) -> Self {
        let mut win = Window::new(bounds, Some("Run Container".to_string()), 0);
        let size = win.state().size;
        let snapshot = values.borrow().clone();
        let inits = [
            snapshot.command,
            snapshot.shell,
            snapshot.user,
            snapshot.workdir,
            snapshot.env,
            snapshot.ports,
            snapshot.volumes,
            snapshot.name,
            snapshot.restart,
            snapshot.memory,
            snapshot.cpu,
            snapshot.network,
            snapshot.labels,
        ];
        let mut field_ids = Vec::with_capacity(LABELS.len());
        // One row per field: label left, input right (visual order =
        // insertion order — the shell-config lesson from bug 1).
        for (i, label) in LABELS.iter().enumerate() {
            let y = 1 + i as i32;
            win.insert_child(Box::new(StaticText::new(
                Rect::new(2, y, 13, y + 1),
                format!("{label}:"),
            )));
            let mut input = InputLine::new(
                Rect::new(14, y, size.x - 2, y + 1),
                255,
                None,
                tv::LimitMode::MaxChars,
            );
            input.set_value(FieldValue::Text(inits[i].clone()));
            field_ids.push(win.insert_child(Box::new(input)));
        }
        let boxes_y = 1 + LABELS.len() as i32;
        let checkboxes = win.insert_child(Box::new(CheckBoxes::new(
            Rect::new(2, boxes_y, size.x - 2, boxes_y + 2),
            vec!["~a~utoremove".to_string(), "~p~rivileged".to_string()],
        )));
        let error = Rc::new(RefCell::new(String::new()));
        win.insert_child(Box::new(HintRow {
            st: ViewState::new(Rect::new(2, boxes_y + 2, size.x - 2, boxes_y + 3)),
            error: Rc::clone(&error),
        }));
        Self {
            win,
            values,
            field_ids,
            checkboxes,
            error,
        }
    }

    fn collect(&mut self) -> RunForm {
        let text = |win: &mut Window, id: tv::ViewId| -> String {
            win.child_mut(id)
                .and_then(|v| v.value())
                .and_then(|fv| match fv {
                    FieldValue::Text(t) => Some(t),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let bits = self
            .win
            .child_mut(self.checkboxes)
            .and_then(|v| v.value())
            .and_then(|fv| match fv {
                FieldValue::Bits(b) => Some(b),
                _ => None,
            })
            .unwrap_or(0);
        let win = &mut self.win;
        RunForm {
            command: text(win, self.field_ids[0]),
            shell: text(win, self.field_ids[1]),
            user: text(win, self.field_ids[2]),
            workdir: text(win, self.field_ids[3]),
            env: text(win, self.field_ids[4]),
            ports: text(win, self.field_ids[5]),
            volumes: text(win, self.field_ids[6]),
            name: text(win, self.field_ids[7]),
            restart: text(win, self.field_ids[8]),
            memory: text(win, self.field_ids[9]),
            cpu: text(win, self.field_ids[10]),
            network: text(win, self.field_ids[11]),
            labels: text(win, self.field_ids[12]),
            autoremove: bits & 1 != 0,
            privileged: bits & 2 != 0,
        }
    }

    fn submit(&mut self, ctx: &mut Context) {
        let form = self.collect();
        if let Err(msg) = validate(&form) {
            *self.error.borrow_mut() = msg;
            return; // stay open — the hint row shows the message
        }
        *self.values.borrow_mut() = form;
        ctx.end_modal(tv::Command::OK);
    }
}

/// The bottom row: the validation error when set, else the key hints.
struct HintRow {
    st: ViewState,
    error: Rc<RefCell<String>>,
}

impl View for HintRow {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let msg = self.error.borrow();
        let (text, role) = if msg.is_empty() {
            (
                " Enter runs the container · Esc cancels · ';' separates list values".to_string(),
                Role::Disabled,
            )
        } else {
            (format!(" {msg}"), Role::Warning)
        };
        ctx.put_str(0, 0, &text, ctx.style(role));
    }
}

#[delegate(to = win)]
impl View for RunDialog {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.win.draw(ctx);
    }

    /// Open with "Command" focused — the default would pick the LAST
    /// inserted selectable child (the checkbox group).
    fn reset_current(&mut self, ctx: &mut Context) {
        self.win.focus_descendant(self.field_ids[0], ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        if let Event::KeyDown(ke) = ev {
            match ke.key {
                Key::Enter => {
                    self.submit(ctx);
                    ev.clear();
                    return;
                }
                Key::Esc => {
                    ctx.end_modal(tv::Command::CANCEL);
                    ev.clear();
                    return;
                }
                _ => {}
            }
        }
        // The modal contract (see `ShellConfigDialog::handle_event`): the
        // answer may arrive as a command as well as a key.
        if let Event::Command(cmd) = ev {
            if *cmd == tv::Command::OK {
                self.submit(ctx);
                ev.clear();
                return;
            }
            if *cmd == tv::Command::CANCEL {
                ctx.end_modal(tv::Command::CANCEL);
                ev.clear();
                return;
            }
        }
        self.win.handle_event(ev, ctx);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tv::event::{KeyEvent, KeyModifiers};

    fn form(ports: &str, volumes: &str) -> RunForm {
        RunForm {
            ports: ports.into(),
            volumes: volumes.into(),
            ..Default::default()
        }
    }

    #[test]
    fn persist_row_is_none_on_an_unedited_submit_whatever_the_config_holds() {
        use crate::domain::config::{ContainerShellConfig, TvdockerConfig};
        // Virgin: nothing recorded anywhere.
        let virgin = TvdockerConfig::default();
        let form = RunForm::prefill_from(&virgin, "nginx");
        assert_eq!(form.persist_row(&virgin, "nginx"), None);
        // Non-virgin WITHOUT an entry for this image must not fall
        // through to a write.
        let cfg = TvdockerConfig {
            latest_shell: Some("fish".into()),
            ..Default::default()
        };
        let form = RunForm::prefill_from(&cfg, "nginx");
        assert_eq!(
            form.persist_row(&cfg, "nginx"),
            None,
            "an existing config without this image's entry must not gain one"
        );
        // Non-virgin WITH the entry the F4 dialog stored.
        let mut cfg = TvdockerConfig::default();
        cfg.images.insert(
            "nginx".into(),
            ContainerShellConfig {
                shell: Some("fish".into()),
                user: Some("bob".into()),
                workdir: Some("/srv".into()),
            },
        );
        let form = RunForm::prefill_from(&cfg, "nginx");
        assert_eq!(form.persist_row(&cfg, "nginx"), None);
    }

    #[test]
    fn persist_row_preserves_the_workdir_the_shell_dialog_stored() {
        use crate::domain::config::{ContainerShellConfig, TvdockerConfig};
        let mut cfg = TvdockerConfig::default();
        cfg.images.insert(
            "nginx".into(),
            ContainerShellConfig {
                shell: Some("fish".into()),
                user: None,
                workdir: Some("/srv/app".into()),
            },
        );
        let mut form = RunForm::prefill_from(&cfg, "nginx");
        form.user = "bob".into(); // the one edit
        let row = form.persist_row(&cfg, "nginx").expect("edited ⇒ write");
        assert_eq!(row.shell.as_deref(), Some("fish"), "unedited shell kept");
        assert_eq!(row.user.as_deref(), Some("bob"));
        assert_eq!(
            row.workdir.as_deref(),
            Some("/srv/app"),
            "the run form never collects workdir — the F4 dialog's value must survive"
        );
    }

    #[test]
    fn persist_row_writes_a_fresh_entry_when_the_image_has_none() {
        use crate::domain::config::{ContainerShellConfig, TvdockerConfig};
        let cfg = TvdockerConfig {
            latest_shell: Some("fish".into()),
            ..Default::default()
        };
        let mut form = RunForm::prefill_from(&cfg, "nginx");
        form.user = "bob".into();
        let row = form.persist_row(&cfg, "nginx").expect("edited ⇒ write");
        assert_eq!(
            row,
            ContainerShellConfig {
                shell: Some("fish".into()),
                user: Some("bob".into()),
                workdir: None,
            },
            "the prefilled shell rides along; no workdir is invented"
        );
    }

    #[test]
    fn validators_accept_the_docker_shapes() {
        assert!(validate(&form("8080:80", "")).is_ok(), "HOST:CONTAINER");
        assert!(validate(&form("80", "")).is_ok(), "bare container port");
        assert!(
            validate(&form("8080:80;9090:90", "/a:/b:ro")).is_ok(),
            "multi-value via the ';' separator"
        );
        assert!(validate(&form("", "")).is_ok(), "everything optional");
    }

    #[test]
    fn validators_reject_bad_ports_and_separator_less_volumes() {
        let err = validate(&form("nope", "")).expect_err("non-numeric");
        assert!(err.contains("nope"), "{err}");
        let err = validate(&form("a:b:80:90", "")).expect_err("4 segments");
        assert!(err.contains("HOST:CONTAINER"), "{err}");
        let err = validate(&form("", "/data")).expect_err("volume needs ':'");
        assert!(err.contains("HOST:CONTAINER"), "{err}");
    }

    #[test]
    fn validators_reject_unparseable_memory_and_cpu() {
        // Reviewer finding 9: `if let Ok` in create_container silently
        // DROPPED unparseable limits — the container ran unlimited with
        // no error anywhere. The form must refuse instead.
        let f = RunForm {
            memory: "lots".into(),
            ..Default::default()
        };
        let err = validate(&f).expect_err("garbage memory");
        assert!(err.contains("lots"), "{err}");
        let f = RunForm {
            cpu: "many".into(),
            ..Default::default()
        };
        let err = validate(&f).expect_err("garbage cpu");
        assert!(err.contains("many"), "{err}");
        // Valid spellings pass, including the CLI `mb` form.
        for memory in ["512m", "512mb", "1g", "2048"] {
            let f = RunForm {
                memory: memory.into(),
                ..Default::default()
            };
            assert!(validate(&f).is_ok(), "memory {memory} must be valid");
        }
        for cpu in ["0.5", "2", "1.25"] {
            let f = RunForm {
                cpu: cpu.into(),
                ..Default::default()
            };
            assert!(validate(&f).is_ok(), "cpu {cpu} must be valid");
        }
    }

    #[test]
    fn validators_name_the_separator_for_cli_habits() {
        // Review Focus #1: docker-CLI muscle memory types commas.
        let err = validate(&form("8080:80,9090:90", "")).expect_err("comma");
        assert!(
            err.contains(';'),
            "the error must name the ';' separator: {err}"
        );
    }

    #[test]
    fn to_opts_joins_multivalue_fields_on_newlines() {
        let f = RunForm {
            ports: "8080:80;9090:90".into(),
            env: "A=1;B=2".into(),
            ..Default::default()
        };
        let opts = f.to_opts("nginx:1.27");
        assert_eq!(opts.image, "nginx:1.27");
        assert_eq!(opts.port_mapping, "8080:80\n9090:90");
        assert_eq!(opts.env_vars, "A=1\nB=2");
    }

    #[test]
    fn tab_walks_every_field_in_visual_order_and_opens_on_command() {
        let values = Rc::new(RefCell::new(RunForm::default()));
        let mut dlg = RunDialog::new(Rect::new(0, 0, 68, 19), values);
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);

        fn selected(dlg: &mut RunDialog) -> Vec<usize> {
            dlg.field_ids
                .iter()
                .position(|id| {
                    dlg.win
                        .child_mut(*id)
                        .is_some_and(|v| v.state().state.selected)
                })
                .into_iter()
                .collect()
        }
        dlg.reset_current(&mut ctx);
        assert_eq!(selected(&mut dlg), vec![0], "opens on Command");

        for want in 1..dlg.field_ids.len() {
            let mut ev = Event::KeyDown(KeyEvent::new(Key::Tab, KeyModifiers::default()));
            dlg.handle_event(&mut ev, &mut ctx);
            assert_eq!(selected(&mut dlg), vec![want], "Tab step {want}");
        }
        // One more Tab reaches the checkboxes (selectable, no field index),
        // then wraps to field 0.
        let mut ev = Event::KeyDown(KeyEvent::new(Key::Tab, KeyModifiers::default()));
        dlg.handle_event(&mut ev, &mut ctx);
        assert!(
            dlg.win
                .child_mut(dlg.checkboxes)
                .is_some_and(|v| v.state().state.selected),
            "Tab reaches the checkbox group"
        );
        let mut ev = Event::KeyDown(KeyEvent::new(Key::Tab, KeyModifiers::default()));
        dlg.handle_event(&mut ev, &mut ctx);
        assert_eq!(selected(&mut dlg), vec![0], "wraps to Command");
    }

    #[test]
    fn submit_with_a_bad_port_shows_the_error_and_stays_open() {
        let values = Rc::new(RefCell::new(RunForm::default()));
        let mut dlg = RunDialog::new(Rect::new(0, 0, 68, 19), values);
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut timers = tv::TimerQueue::new();
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            dlg.reset_current(&mut ctx);

            // Type "nope" into the Ports field (Tab to it: Command→…→Ports).
            for _ in 0..6 {
                let mut ev = Event::KeyDown(KeyEvent::new(Key::Tab, KeyModifiers::default()));
                dlg.handle_event(&mut ev, &mut ctx);
            }
            for ch in "nope".chars() {
                let mut ev = Event::KeyDown(KeyEvent::from(Key::Char(ch)));
                dlg.handle_event(&mut ev, &mut ctx);
            }
            let mut ev = Event::KeyDown(KeyEvent::new(Key::Enter, KeyModifiers::default()));
            dlg.handle_event(&mut ev, &mut ctx);
        }
        assert!(
            !deferred
                .iter()
                .any(|d| matches!(d, tv::Deferred::EndModal(_))),
            "a validation error must keep the dialog open (no EndModal)"
        );
        {
            let mut timers = tv::TimerQueue::new();
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::KeyDown(KeyEvent::new(Key::Esc, KeyModifiers::default()));
            dlg.handle_event(&mut ev, &mut ctx);
        }
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, tv::Deferred::EndModal(c) if *c == tv::Command::CANCEL)),
            "Esc → EndModal(CANCEL)"
        );
        let rendered = crate::testutil::text(&crate::testutil::render_view(68, 19, &mut dlg));
        assert!(
            rendered.contains("nope"),
            "the message names the value: {rendered:?}"
        );
    }

    #[test]
    fn submit_collects_values_checkboxes_and_ends_ok() {
        let values = Rc::new(RefCell::new(RunForm::default()));
        let mut dlg = RunDialog::new(Rect::new(0, 0, 68, 19), values.clone());
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut timers = tv::TimerQueue::new();
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            dlg.reset_current(&mut ctx);

            for ch in "nginx".chars() {
                let mut ev = Event::KeyDown(KeyEvent::from(Key::Char(ch)));
                dlg.handle_event(&mut ev, &mut ctx);
            }
            // Alt+a — the checkbox group's documented accelerator (the
            // `~a~` hot marker) — works from anywhere; Space needs the
            // live program focus chain, which a unit harness lacks.
            let alt_a = Event::KeyDown(KeyEvent::new(
                Key::Char('a'),
                KeyModifiers {
                    alt: true,
                    ..KeyModifiers::default()
                },
            ));
            let mut ev = alt_a;
            dlg.handle_event(&mut ev, &mut ctx);

            let mut ev = Event::KeyDown(KeyEvent::new(Key::Enter, KeyModifiers::default()));
            dlg.handle_event(&mut ev, &mut ctx);
        }
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, tv::Deferred::EndModal(c) if *c == tv::Command::OK)),
            "Enter → EndModal(OK)"
        );
        let v = values.borrow();
        assert_eq!(v.command, "nginx", "the typed Command field");
        assert!(v.autoremove, "the checkbox bit round-trips");
        assert!(!v.privileged);
    }
}
