//! Modal dialogs launched by view code via `ctx.request_exec_view`
//! (modals). Data travels through SHARED
//! `Rc` state, not through the close command — tvision-rs deliberately
//! ships no modal data-back path (`context.rs:1490`).

use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key};
use tv::{
    Command, Context, FieldValue, InputLine, LimitMode, Rect, StaticText, View, ViewState, Window,
    delegate,
};

/// The three editable shell fields — created by the
/// desktop, mutated by the dialog, read after `Command::OK`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellForm {
    pub shell: String,
    pub user: String,
    pub workdir: String,
}

/// Vendored from oh-my-docker's `util::image_base_name` (the per-image
/// config key), corrected: only the TAG is stripped — the last colon
/// segment counts as a tag when it carries no `/`, so a registry port
/// (`localhost:5000/nginx:1.27`) survives into the key instead of
/// collapsing every image behind that registry onto one entry.
pub fn image_base_name(image: &str) -> &str {
    match image.rsplit_once(':') {
        Some((base, last)) if !last.contains('/') => base,
        _ => image,
    }
}

fn nonempty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

impl ShellForm {
    /// The prefill chain against the REAL config (sync read of a ~kB
    /// file).
    pub fn prefill(container: &str, image: &str) -> ShellForm {
        let (config, _err) = crate::domain::config::TvdockerConfig::load();
        Self::prefill_from(&config, container, image)
    }

    /// The chain itself (vendored tvdocker
    /// `reducers/navigation.rs:47-73`): per-container → per-image →
    /// `latest_shell` → `"bash"`; user/workdir fall back to empty
    /// (= container default).
    pub fn prefill_from(
        config: &crate::domain::config::TvdockerConfig,
        container: &str,
        image: &str,
    ) -> ShellForm {
        let per_container = config.containers.get(container);
        let per_image = config.images.get(image);
        let shell = per_container
            .and_then(|c| c.shell.clone())
            .or_else(|| per_image.and_then(|i| i.shell.clone()))
            .or_else(|| config.latest_shell.clone())
            .unwrap_or_else(|| "bash".to_string());
        let user = per_container
            .and_then(|c| c.user.clone())
            .or_else(|| per_image.and_then(|i| i.user.clone()))
            .unwrap_or_default();
        let workdir = per_container
            .and_then(|c| c.workdir.clone())
            .or_else(|| per_image.and_then(|i| i.workdir.clone()))
            .unwrap_or_default();
        ShellForm {
            shell,
            user,
            workdir,
        }
    }

    /// Persist (load-modify-save — vendored tvdocker
    /// `reducers/shell.rs:29-60`: latest + per-container + per-image).
    /// Writes ONLY when something actually changed: an unedited Enter
    /// (tests, impatient users) never touches the user's file.
    /// `Ok(true)` = written.
    /// `Err(String)` carries the user-facing reason, not an error chain.
    pub fn save_if_changed(&self, container: &str, image: &str) -> Result<bool, String> {
        let (mut config, err) = crate::domain::config::TvdockerConfig::load();
        if let Some(e) = err {
            return Err(e); // a broken file must be fixed by hand first
        }
        let row = crate::domain::config::ContainerShellConfig {
            shell: Some(self.shell.clone()),
            user: nonempty(&self.user),
            workdir: nonempty(&self.workdir),
        };
        let image_key = image_base_name(image).to_string();
        let unchanged = config.latest_shell.as_deref() == Some(self.shell.as_str())
            && config.containers.get(container) == Some(&row)
            && config.images.get(&image_key) == Some(&row);
        if unchanged {
            return Ok(false);
        }
        // an UNEDITED Enter must never touch the user's
        // file — and must not CREATE one either. On a virgin config the
        // three `unchanged` legs can never all match (nothing is
        // recorded yet), so the form is compared against the very prefill
        // that config yields: identical means there is nothing to
        // persist. (The integration suite additionally retargets the
        // config at a temp file — `TvdockerConfig::set_path_override` —
        // so its F4 round trip never even reads the real file.)
        if !crate::domain::config::TvdockerConfig::path().exists()
            && *self == Self::prefill_from(&config, container, image)
        {
            return Ok(false);
        }
        config.latest_shell = Some(self.shell.clone());
        config.containers.insert(container.to_string(), row.clone());
        config.images.insert(image_key, row);
        config.save()?;
        Ok(true)
    }
}

/// The shell-config dialog (modal; §5: Enter submits, Esc
/// cancels): a `Window` with three `InputLine`s + `StaticText` labels
/// and per-field hints. Tab cycles the fields (the window's own focus
/// wrap — static text is not selectable). Enter collects into the
/// shared `values` and ends the modal with `Command::OK`; Esc cancels.
pub struct ShellConfigDialog {
    win: Window,
    values: Rc<RefCell<ShellForm>>,
    fields: [tv::ViewId; 3],
}

impl ShellConfigDialog {
    /// `values` is prefilled by the caller (the desktop) and read by
    /// it after `Command::OK`.
    pub fn new(bounds: Rect, values: Rc<RefCell<ShellForm>>, container: &str) -> Self {
        let short: String = container.chars().take(12).collect();
        let mut win = Window::new(bounds, Some(format!("Shell Config — {short}")), 0);
        let size = win.state().size;
        let labels = ["Shell", "User", "Workdir"];
        let hints = [
            "sh · bash · /bin/zsh",
            "empty = container default · host · root · user:group",
            "empty = container default · / · /app",
        ];
        let snapshot = values.borrow().clone();
        let inits = [snapshot.shell, snapshot.user, snapshot.workdir];

        // Insert the INPUTS in VISUAL order (Shell, User, Workdir): the
        // window walks focus in insertion order, so this makes Tab run
        // top-to-bottom. The initial focus on "Shell" is set explicitly
        // by `reset_current` (the default picks the last-inserted child,
        // which would be Workdir).
        // Field rows: y = 2 / 5 / 8 (window is 60×10, frame at 0/9).
        let id0 = {
            let mut input = InputLine::new(
                Rect::new(14, 2, size.x - 2, 3),
                255,
                None,
                LimitMode::MaxChars,
            );
            input.set_value(FieldValue::Text(inits[0].clone()));
            win.insert_child(Box::new(input))
        };
        let id1 = {
            let mut input = InputLine::new(
                Rect::new(14, 5, size.x - 2, 6),
                255,
                None,
                LimitMode::MaxChars,
            );
            input.set_value(FieldValue::Text(inits[1].clone()));
            win.insert_child(Box::new(input))
        };
        let id2 = {
            let mut input = InputLine::new(
                Rect::new(14, 8, size.x - 2, 9),
                255,
                None,
                LimitMode::MaxChars,
            );
            input.set_value(FieldValue::Text(inits[2].clone()));
            win.insert_child(Box::new(input))
        };
        // Labels: a hint row + a "Label:" row per block (y = 1/4/7 →
        // fields at y = 2/5/8).
        for (i, label) in labels.iter().enumerate() {
            let hint_y = 1 + (i as i32) * 3;
            let field_y = hint_y + 1;
            win.insert_child(Box::new(StaticText::new(
                Rect::new(2, hint_y, size.x - 2, hint_y + 1),
                hints[i],
            )));
            win.insert_child(Box::new(StaticText::new(
                Rect::new(2, field_y, 14, field_y + 1),
                format!("{label}:"),
            )));
        }
        Self {
            win,
            values,
            fields: [id0, id1, id2],
        }
    }
}

impl ShellConfigDialog {
    /// Collect the three fields into the shared form, then end the modal
    /// with `Command::OK` (the desktop reads `values` afterwards — no
    /// data-back path, see the module doc).
    fn submit(&mut self, ctx: &mut Context) {
        let mut values = self.values.borrow_mut();
        for (i, id) in self.fields.iter().enumerate() {
            let text = self
                .win
                .child_mut(*id)
                .and_then(|v| v.value())
                .and_then(|fv| match fv {
                    FieldValue::Text(t) => Some(t),
                    _ => None,
                })
                .unwrap_or_default();
            match i {
                0 => values.shell = text,
                1 => values.user = text,
                _ => values.workdir = text,
            }
        }
        ctx.end_modal(Command::OK);
    }
}

#[delegate(to = win)]
impl View for ShellConfigDialog {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }
    fn draw(&mut self, ctx: &mut tv::DrawCtx) {
        self.win.draw(ctx);
    }

    /// Open with "Shell" focused. The inputs are inserted in visual
    /// order (so the Tab walk runs top-to-bottom), which makes the
    /// default `Window::reset_current` pick the last-inserted child
    /// (Workdir); name field 0 explicitly instead.
    fn reset_current(&mut self, ctx: &mut Context) {
        self.win.focus_descendant(self.fields[0], ctx);
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
                    ctx.end_modal(Command::CANCEL);
                    ev.clear();
                    return;
                }
                _ => {}
            }
        }
        // The modal contract: `Command::OK` submits and `Command::CANCEL`
        // dismisses, however the answer arrives (a menu, a scripted key
        // stream, or the framework's own test idiom).
        if let Event::Command(cmd) = ev {
            if *cmd == Command::OK {
                self.submit(ctx);
                ev.clear();
                return;
            }
            if *cmd == Command::CANCEL {
                ctx.end_modal(Command::CANCEL);
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

/// A one-field modal prompt: rename / new dir / new file /
/// goto-path all reuse it. Enter collects into the shared `value` and
/// ends the modal with `Command::OK`; Esc cancels without writing
/// (shared-form pattern).
///
/// The prefill is intentionally NOT select-all-on-focus (the `InputLine`
/// default would replace it on the first keystroke): typing APPENDS, so
/// a rename user edits the tail or presses Backspace first.
pub struct InputDialog {
    win: Window,
    value: Rc<RefCell<String>>,
    input_id: tv::ViewId,
}

impl InputDialog {
    /// `value` is prefilled by the caller and read by it after OK.
    pub fn new(bounds: Rect, title: &str, label: &str, value: Rc<RefCell<String>>) -> Self {
        let mut win = Window::new(bounds, Some(title.to_string()), 0);
        let size = win.state().size;
        let y = 2;
        win.insert_child(Box::new(StaticText::new(
            Rect::new(2, y, 12, y + 1),
            format!("{label}:"),
        )));
        let mut input = InputLine::new(
            Rect::new(13, y, size.x - 2, y + 1),
            255,
            None,
            LimitMode::MaxChars,
        );
        input.set_value(FieldValue::Text(value.borrow().clone()));
        // `InputLine` selects all on `set_value` (ValuePosition::End) AND
        // on focus by default — the first keystroke would REPLACE the
        // prefill. Park the caret at the end (which clears the selection)
        // and stop focus from re-selecting, so typing appends instead.
        input.end();
        input.set_select_all_on_focus(false);
        let input_id = win.insert_child(Box::new(input));
        win.insert_child(Box::new(StaticText::new(
            Rect::new(2, y + 2, size.x - 2, y + 3),
            "Enter: ok · Esc: cancel",
        )));
        Self {
            win,
            value,
            input_id,
        }
    }

    /// The single InputLine's text (`None` on any other field kind).
    fn read_input(&mut self) -> Option<String> {
        self.win
            .child_mut(self.input_id)
            .and_then(|v| v.value())
            .and_then(|fv| match fv {
                FieldValue::Text(t) => Some(t),
                _ => None,
            })
    }
}

#[delegate(to = win)]
impl View for InputDialog {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }
    fn draw(&mut self, ctx: &mut tv::DrawCtx) {
        self.win.draw(ctx);
    }

    fn reset_current(&mut self, ctx: &mut Context) {
        // Focus the input line (the only selectable child).
        self.win.focus_descendant(self.input_id, ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        if let Event::KeyDown(ke) = ev {
            match ke.key {
                Key::Enter => {
                    if let Some(text) = self.read_input() {
                        *self.value.borrow_mut() = text;
                    }
                    ctx.end_modal(tv::Command::OK);
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
                if let Some(text) = self.read_input() {
                    *self.value.borrow_mut() = text;
                }
                ctx.end_modal(tv::Command::OK);
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
pub struct ColumnPickerDialog {
    win: Window,
    flags: Rc<RefCell<Vec<bool>>>,
    labels: &'static [&'static str],
    cursor: Rc<RefCell<usize>>,
}

impl ColumnPickerDialog {
    /// Build the dialog; `flags` is the window's fresh copy of its
    /// applied visibility (Review Focus #5 — cancel must not leak).
    pub fn new(
        bounds: Rect,
        labels: &'static [&'static str],
        flags: Rc<RefCell<Vec<bool>>>,
    ) -> Self {
        let mut win = Window::new(bounds, Some("Columns".to_string()), 0);
        let rows = PickerRowsView {
            st: ViewState::new(Rect::new(
                1,
                1,
                bounds.b.x - bounds.a.x - 1,
                bounds.b.y - bounds.a.y - 1,
            )),
            labels,
            flags: Rc::clone(&flags),
            cursor: Rc::new(RefCell::new(0)),
        };
        // single selectable child: focus lands there, keys reach the
        // wrapper first (top-down), Tab keeps it (one child wraps to
        // itself).
        let cursor = Rc::clone(&rows.cursor);
        win.insert_child(Box::new(rows));
        Self {
            win,
            flags,
            labels,
            cursor,
        }
    }
}

/// The checkbox rows + footer (draw-only; the wrapper owns the keys).
struct PickerRowsView {
    st: ViewState,
    labels: &'static [&'static str],
    flags: Rc<RefCell<Vec<bool>>>,
    cursor: Rc<RefCell<usize>>,
}

impl View for PickerRowsView {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut tv::DrawCtx) {
        let flags = self.flags.borrow();
        let cursor = self.cursor.borrow();
        for (i, label) in self.labels.iter().enumerate() {
            let mark = if flags.get(i).copied().unwrap_or(false) {
                "[x]"
            } else {
                "[ ]"
            };
            let style = if i == *cursor {
                ctx.style(tv::Role::ListFocused)
            } else {
                ctx.style(tv::Role::ListNormal)
            };
            ctx.put_str(1, 1 + i as i32, &format!("{mark} {label}"), style);
        }
        ctx.put_str(
            1,
            self.st.size.y - 2,
            "Space toggle · Enter apply · Esc cancel",
            ctx.style(tv::Role::Info),
        );
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[delegate(to = win)]
impl View for ColumnPickerDialog {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }
    fn draw(&mut self, ctx: &mut tv::DrawCtx) {
        self.win.draw(ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        if let Event::KeyDown(ke) = ev {
            match ke.key {
                Key::Down => {
                    let mut c = self.cursor.borrow_mut();
                    *c = (*c + 1) % self.labels.len().max(1);
                    ev.clear();
                    return;
                }
                Key::Up => {
                    let mut c = self.cursor.borrow_mut();
                    *c = (*c + self.labels.len().saturating_sub(1)) % self.labels.len().max(1);
                    ev.clear();
                    return;
                }
                Key::Char(' ') => {
                    let c = *self.cursor.borrow();
                    if let Some(f) = self.flags.borrow_mut().get_mut(c) {
                        *f = !*f;
                    }
                    ev.clear();
                    return;
                }
                Key::Enter => {
                    ctx.end_modal(Command::OK);
                    ev.clear();
                    return;
                }
                Key::Esc => {
                    ctx.end_modal(Command::CANCEL);
                    ev.clear();
                    return;
                }
                _ => {}
            }
        }
        // The modal contract (see `ShellConfigDialog::handle_event`): the
        // answer may arrive as a command as well as a key.
        if let Event::Command(cmd) = ev {
            if *cmd == Command::OK {
                ctx.end_modal(Command::OK);
                ev.clear();
                return;
            }
            if *cmd == Command::CANCEL {
                ctx.end_modal(Command::CANCEL);
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
mod shell_form_tests {
    use super::*;
    use crate::domain::config::{ContainerShellConfig, TvdockerConfig};

    fn config() -> TvdockerConfig {
        let mut c = TvdockerConfig {
            latest_shell: Some("fish".into()),
            ..Default::default()
        };
        c.containers.insert(
            "web".to_string(),
            ContainerShellConfig {
                shell: Some("zsh".into()),
                user: Some("1000".into()),
                workdir: Some("/app".into()),
            },
        );
        c.images.insert(
            "nginx:1.27".to_string(),
            ContainerShellConfig {
                shell: Some("sh".into()),
                user: None,
                workdir: None,
            },
        );
        c
    }

    #[test]
    fn prefill_follows_the_vendored_fallback_chain() {
        let c = config();
        // per-container wins…
        let f = ShellForm::prefill_from(&c, "web", "nginx:1.27");
        assert_eq!(f.shell, "zsh");
        assert_eq!(f.user, "1000");
        assert_eq!(f.workdir, "/app");
        // …then per-image…
        let f = ShellForm::prefill_from(&c, "other", "nginx:1.27");
        assert_eq!(f.shell, "sh");
        assert_eq!(f.user, "", "user falls back to container default");
        // …then latest_shell…
        let f = ShellForm::prefill_from(&c, "other", "unknown:1");
        assert_eq!(f.shell, "fish");
        // …then "bash".
        let mut c = config();
        c.latest_shell = None;
        let f = ShellForm::prefill_from(&c, "other", "unknown:1");
        assert_eq!(f.shell, "bash", "final default");
        let _ = f;
    }

    #[test]
    fn image_base_name_strips_the_tag() {
        assert_eq!(image_base_name("nginx:1.27"), "nginx");
        assert_eq!(image_base_name("reg.io/ns/app:1"), "reg.io/ns/app");
    }

    #[test]
    fn image_base_name_keeps_registry_ports_out_of_the_config_key() {
        // The config key is per-IMAGE: a dev registry's port must not
        // swallow the key.
        assert_eq!(
            image_base_name("localhost:5000/nginx:1.27"),
            "localhost:5000/nginx",
            "only the TAG is stripped, never a registry port"
        );
        assert_eq!(
            image_base_name("localhost:5000/nginx"),
            "localhost:5000/nginx",
            "no tag after the port ⇒ the reference is the key"
        );
        assert_eq!(image_base_name("nginx"), "nginx");
    }

    #[test]
    fn both_dialogs_report_no_help_ctx_so_a_live_enter_reaches_them() {
        use tv::HelpCtx;
        // While a modal runs, the pump re-targets the status line to the
        // MODAL's help ctx; NO_CONTEXT ⇒ the universal def, which binds
        // no Enter/Del. So a human's Enter flows raw into the dialog and
        // submits it — verified live. (A pre-queued harness stream never
        // idles between events, so it keeps the Containers def and must
        // answer with Command::OK instead — tests/resource_actions.rs.)
        let values = Rc::new(RefCell::new(ShellForm {
            shell: "sh".into(),
            user: String::new(),
            workdir: String::new(),
        }));
        let dlg = ShellConfigDialog::new(Rect::new(0, 0, 60, 10), values, "web");
        assert_eq!(
            dlg.get_help_ctx(),
            HelpCtx::NO_CONTEXT,
            "shell config dialog"
        );

        let flags = Rc::new(RefCell::new(vec![true, false]));
        let picker = ColumnPickerDialog::new(Rect::new(0, 0, 40, 12), &["NAME", "IMAGE"], flags);
        assert_eq!(
            picker.get_help_ctx(),
            HelpCtx::NO_CONTEXT,
            "column picker dialog"
        );
    }

    /// Tab/Shift+Tab must walk the fields in visual order and open on
    /// "Shell". Root cause of the reversed-navigation bug: the inputs
    /// were inserted in REVERSE to make `reset_current` (which focuses
    /// the last-inserted selectable child) land on Shell — but the
    /// window's Tab traversal is INSERTION order, so the walk ran
    /// bottom-to-top. Fix: insert in visual order + focus Shell via the
    /// `reset_current` open hook.
    #[test]
    fn tab_and_shift_tab_walk_the_fields_in_visual_order() {
        use std::collections::VecDeque;
        use tv::event::{KeyEvent, KeyModifiers};
        use tv::{Deferred, TimerQueue};

        let values = Rc::new(RefCell::new(ShellForm {
            shell: "sh".into(),
            user: String::new(),
            workdir: String::new(),
        }));
        let mut dlg = ShellConfigDialog::new(Rect::new(0, 0, 60, 10), values, "web");
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut timers = TimerQueue::new();
        let mut deferred: Vec<Deferred> = Vec::new();
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);

        // The selected field, by index (0 = Shell, 1 = User, 2 = Workdir).
        fn current(dlg: &mut ShellConfigDialog) -> Vec<usize> {
            (0..3)
                .filter(|i| {
                    dlg.win
                        .child_mut(dlg.fields[*i])
                        .is_some_and(|v| v.state().state.selected)
                })
                .collect()
        }
        fn send_tab(dlg: &mut ShellConfigDialog, ctx: &mut Context, shift: bool) {
            let mut ev = Event::KeyDown(KeyEvent::new(
                Key::Tab,
                KeyModifiers {
                    shift,
                    ..KeyModifiers::default()
                },
            ));
            dlg.handle_event(&mut ev, ctx);
        }

        // The program's `exec_view` open hook.
        dlg.reset_current(&mut ctx);
        assert_eq!(current(&mut dlg), vec![0], "opening focuses Shell");

        send_tab(&mut dlg, &mut ctx, false);
        assert_eq!(current(&mut dlg), vec![1], "Tab: Shell → User");
        send_tab(&mut dlg, &mut ctx, false);
        assert_eq!(current(&mut dlg), vec![2], "Tab: User → Workdir");
        send_tab(&mut dlg, &mut ctx, false);
        assert_eq!(current(&mut dlg), vec![0], "Tab: Workdir wraps to Shell");

        send_tab(&mut dlg, &mut ctx, true);
        assert_eq!(current(&mut dlg), vec![2], "Shift+Tab: Shell → Workdir");
        send_tab(&mut dlg, &mut ctx, true);
        assert_eq!(current(&mut dlg), vec![1], "Shift+Tab: Workdir → User");
        send_tab(&mut dlg, &mut ctx, true);
        assert_eq!(current(&mut dlg), vec![0], "Shift+Tab: User wraps to Shell");
    }
}

#[cfg(test)]
mod input_dialog_tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn input_dialog_prefills_collects_and_cancels() {
        use tv::event::{KeyEvent, KeyModifiers};
        use tv::{Deferred, TimerQueue};
        let value = Rc::new(RefCell::new("/var/log".to_string()));
        let mut dlg = InputDialog::new(Rect::new(0, 0, 50, 7), "Go to", "Path", Rc::clone(&value));
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut deferred: Vec<Deferred> = Vec::new();
        {
            let mut timers = TimerQueue::new();
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            dlg.reset_current(&mut ctx);
            // The prefilled text is selected-to-end; type a suffix.
            for ch in "/sub".chars() {
                let mut ev = Event::KeyDown(KeyEvent::from(Key::Char(ch)));
                dlg.handle_event(&mut ev, &mut ctx);
            }
            let mut ev = Event::KeyDown(KeyEvent::new(Key::Enter, KeyModifiers::default()));
            dlg.handle_event(&mut ev, &mut ctx);
        }
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, tv::Deferred::EndModal(c) if *c == tv::Command::OK)),
            "Enter → EndModal(OK)"
        );
        assert_eq!(value.borrow().as_str(), "/var/log/sub");

        // Cancel leaves the shared value untouched.
        value.borrow_mut().clear();
        let mut dlg = InputDialog::new(Rect::new(0, 0, 50, 7), "Rename", "New name", value.clone());
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut deferred: Vec<Deferred> = Vec::new();
        {
            let mut timers = TimerQueue::new();
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            dlg.reset_current(&mut ctx);
            for ch in "junk".chars() {
                let mut ev = Event::KeyDown(KeyEvent::from(Key::Char(ch)));
                dlg.handle_event(&mut ev, &mut ctx);
            }
            let mut ev = Event::KeyDown(KeyEvent::new(Key::Esc, KeyModifiers::default()));
            dlg.handle_event(&mut ev, &mut ctx);
        }
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, tv::Deferred::EndModal(c) if *c == tv::Command::CANCEL)),
            "Esc → EndModal(CANCEL)"
        );
        assert!(value.borrow().is_empty(), "cancel must not write back");
    }
}
