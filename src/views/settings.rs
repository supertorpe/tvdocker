//! The Settings window (settings): tvision-rs's editor over
//! `~/.config/tvdocker/tvdocker.toml`. Save writes the file, reports the
//! outcome on the status line, and asks every status line to rebuild (§5).
use crate::core::{AppCore, AppMsg};
use crate::domain::config::TvdockerConfig;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use tv::event::Event;
use tv::{Command, Context, EditWindow, FileEditor, Rect, View, ViewState, delegate};

/// Status-line context for the Settings window.
pub const HELP_CTX: tv::HelpCtx = tv::HelpCtx::custom("tvdocker.settings");

/// `EditWindow` renders no frame number for `0` (settings).
const SETTINGS_NUMBER: i16 = 0;

/// The Settings window: a thin wrapper over tvision-rs's `EditWindow` that
/// owns the save policy (§6) and the dirty-close guard (§11/§3).
pub struct SettingsWindow {
    win: EditWindow,
    core: Rc<RefCell<AppCore>>,
    open: Rc<RefCell<bool>>,
    /// The file this window edits and re-reads after a save.
    path: PathBuf,
    /// A close is waiting on the discard confirmation.
    awaiting_close: bool,
    /// The answer of the last message box (the `ExplorerWindow` route).
    modal_answer: Option<Command>,
}

/// Build the window over the user's config path (settings). An absent
/// file starts an empty buffer — `EditWindow` loads it immediately.
pub fn build(bounds: Rect, core: Rc<RefCell<AppCore>>, open: Rc<RefCell<bool>>) -> SettingsWindow {
    build_at(bounds, core, open, TvdockerConfig::path())
}

/// [`build`] over an explicit path — a test seam. A process-global config
/// override would poison every other config-reading test in the binary (the
/// `init_status_line_with` precedent), so the tests point one window at a
/// scratch file instead.
fn build_at(
    bounds: Rect,
    core: Rc<RefCell<AppCore>>,
    open: Rc<RefCell<bool>>,
    path: PathBuf,
) -> SettingsWindow {
    let mut win = EditWindow::new(bounds, Some(path.clone()), SETTINGS_NUMBER);
    win.state_mut().help_ctx = HELP_CTX;
    SettingsWindow {
        win,
        core,
        open,
        path,
        awaiting_close: false,
        modal_answer: None,
    }
}

impl SettingsWindow {
    /// The hosted [`FileEditor`] (`EditWindow` keeps only its id).
    fn editor_mut(&mut self) -> Option<&mut FileEditor> {
        self.win
            .window
            .child_mut(self.win.editor_id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<FileEditor>())
    }

    /// The editor's dirty flag, or `false` when there is no editor yet.
    fn is_modified(&mut self) -> bool {
        self.editor_mut().is_some_and(|fe| fe.editor.modified())
    }

    /// F2: write the file, report the outcome, and live-reload.
    fn save(&mut self, ctx: &mut Context) {
        let saved = self.editor_mut().is_some_and(|fe| fe.save_file(ctx));
        if !saved {
            self.core
                .borrow_mut()
                .apply(AppMsg::TaskError("Settings could not be saved".into()));
            return;
        }
        // Re-read the file we just wrote: report a parse error, any override
        // that is either unparsable (§6 row three) or a fixed key (§7.4) —
        // a typo must never silently do nothing — and, on success, what the
        // overrides actually did (§7.1 unwired, §7.2 first-chord-only).
        let (cfg, parse_err) = TvdockerConfig::load_from(&self.path);
        let mut problems = crate::domain::config::unparsable_keybindings(&cfg.keybindings);
        problems.extend(crate::domain::config::validate_keybindings(
            &cfg.keybindings,
        ));
        // Bad theme colours/roles and a `theme =` naming nothing are reported
        // exactly like a bad binding — a typo must never silently do nothing.
        problems.extend(cfg.validate_themes());
        let notes = crate::domain::config::keybinding_notes(&cfg.keybindings);
        let mut core = self.core.borrow_mut();
        if let Some(err) = parse_err {
            core.apply(AppMsg::TaskError(err));
            return;
        }
        if !problems.is_empty() {
            let mut parts = vec![format!(
                "ignored: {}",
                problems
                    .iter()
                    .map(|(f, v)| format!("{f} = {v}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            )];
            parts.extend(notes);
            core.apply(AppMsg::TaskError(parts.join("; ")));
        } else if !notes.is_empty() {
            core.apply(AppMsg::Status(format!(
                "Settings saved — {}",
                notes.join("; ")
            )));
        } else {
            core.apply(AppMsg::Status("Settings saved".into()));
        }
        drop(core);
        ctx.broadcast(crate::app::CMD_RELOAD_STATUS, None);
    }

    /// The discard path: drop the in-memory edits so tv's own `FileEditor::valid`
    /// sees a clean buffer and does not prompt a second time.
    fn discard_edits(&mut self) {
        if let Some(fe) = self.editor_mut() {
            let _ = fe.load_file();
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        // The reopen-vs-focus flag (the Logs/Details/Explorer pattern).
        *self.open.borrow_mut() = false;
    }
}

#[delegate(to = win)]
impl View for SettingsWindow {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // F2 is the save key (settings "the action for this
        // window"). Nothing in the status defs binds F2 for this window's
        // help ctx, so the window posts its own command.
        if matches!(
            ev,
            Event::KeyDown(ke)
                if ke.key == tv::event::Key::F(2)
                    && !ke.modifiers.ctrl
                    && !ke.modifiers.alt
                    && !ke.modifiers.shift
        ) {
            *ev = Event::Command(crate::app::CMD_SETTINGS_SAVE);
        }
        if let Event::Command(cmd) = ev {
            if *cmd == crate::app::CMD_SETTINGS_SAVE {
                self.save(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_SETTINGS_CLOSE_CONFIRM {
                let discard = self.modal_answer == Some(Command::YES);
                self.modal_answer = None;
                self.awaiting_close = false;
                if discard {
                    *self.open.borrow_mut() = false;
                    self.discard_edits();
                    // Let the CLOSE through so the desktop removes the window;
                    // the buffer is clean, so the editor does not re-prompt.
                    let mut close = Event::Command(Command::CLOSE);
                    self.win.handle_event(&mut close, ctx);
                }
                ev.clear();
                return;
            }
            if *cmd == Command::CLOSE {
                if self.is_modified() {
                    self.awaiting_close = true;
                    let requester = self.state().id();
                    ctx.request_message_box(
                        "Discard unsaved settings?".to_string(),
                        tv::MessageBoxKind::Confirmation,
                        tv::MessageBoxButtons::yes_no(),
                        requester,
                        Some(crate::app::CMD_SETTINGS_CLOSE_CONFIRM),
                    );
                    ev.clear();
                    return;
                }
                *self.open.borrow_mut() = false;
            }
        }
        self.win.handle_event(ev, ctx);
    }

    fn set_modal_answer(&mut self, cmd: Command) {
        self.modal_answer = Some(cmd);
    }

    /// Hand the pump a concrete reach — nothing downcasts this window
    /// (the save key is an intercepted `Command`, not a `FileEditor` hop).
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
impl SettingsWindow {
    /// Test hook: is a dirty-close confirmation pending?
    fn is_awaiting_close_for_test(&self) -> bool {
        self.awaiting_close
    }

    /// Test hook: reload the buffer from disk (the constructor loaded before
    /// the test wrote the file).
    fn reload_editor_text_for_test(&mut self) {
        if let Some(fe) = self.editor_mut() {
            let _ = fe.load_file();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AppCore;
    use crate::domain::config::TvdockerConfig;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use tv::event::Event;
    use tv::{Command, Deferred, Rect, TimerQueue, View};

    /// A private scratch dir + config path; no process-global override, so
    /// these unit tests never poison the lib-test binary's config path.
    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("tvdocker-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("tvdocker.toml");
        (dir, path)
    }

    fn window(path: PathBuf) -> (SettingsWindow, Rc<RefCell<AppCore>>, Rc<RefCell<bool>>) {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let open = Rc::new(RefCell::new(true));
        let w = build_at(
            Rect::new(0, 0, 76, 23),
            Rc::clone(&core),
            Rc::clone(&open),
            path,
        );
        (w, core, open)
    }

    fn make_dirty(w: &mut SettingsWindow) {
        crate::testutil::with_ctx(|ctx| {
            w.editor_mut()
                .expect("the editor child")
                .editor
                .insert_text(b"# edited", false, ctx);
        });
    }

    fn close(w: &mut SettingsWindow) {
        crate::testutil::with_ctx(|ctx| {
            let mut ev = Event::Command(Command::CLOSE);
            w.handle_event(&mut ev, ctx);
        });
    }

    /// §11: closing with unsaved edits confirms; a clean buffer just closes.
    #[test]
    fn closing_with_unsaved_edits_confirms() {
        let (dir, path) = scratch("dirty");
        let (mut w, _core, open) = window(path);
        make_dirty(&mut w);
        close(&mut w);
        assert!(*open.borrow(), "still open until answered");
        assert!(w.is_awaiting_close_for_test(), "a confirm is pending");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn closing_a_clean_buffer_does_not_confirm() {
        let (dir, path) = scratch("clean");
        let (mut w, _core, open) = window(path);
        close(&mut w);
        assert!(!*open.borrow(), "the slot is released immediately");
        assert!(!w.is_awaiting_close_for_test(), "no confirm was needed");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn answering_no_keeps_the_window_open() {
        let (dir, path) = scratch("no");
        let (mut w, _core, open) = window(path);
        make_dirty(&mut w);
        close(&mut w);
        w.set_modal_answer(Command::NO);
        crate::testutil::with_ctx(|ctx| {
            let mut ev = Event::Command(crate::app::CMD_SETTINGS_CLOSE_CONFIRM);
            w.handle_event(&mut ev, ctx);
        });
        assert!(*open.borrow(), "No keeps it open");
        assert!(!w.is_awaiting_close_for_test(), "the confirm was answered");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The discard path must not let tv's own `FileEditor::valid` prompt a
    /// second time (a double prompt would ship as "Yes then Save?").
    #[test]
    fn answering_yes_discards_and_closes_without_a_second_prompt() {
        let (dir, path) = scratch("yes");
        let (mut w, _core, open) = window(path);
        make_dirty(&mut w);
        close(&mut w);
        w.set_modal_answer(Command::YES);
        let mut out = std::collections::VecDeque::new();
        let mut timers = TimerQueue::new();
        let mut deferred: Vec<Deferred> = Vec::new();
        {
            let mut ctx = tv::Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::Command(crate::app::CMD_SETTINGS_CLOSE_CONFIRM);
            w.handle_event(&mut ev, &mut ctx);
        }
        assert!(!*open.borrow(), "discarded and closed");
        assert!(
            !deferred
                .iter()
                .any(|d| matches!(d, Deferred::OpenMessageBox { .. })),
            "no second prompt from the editor's own valid()"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// §6: saving reports the outcome. A TOML that parses cleanly says so.
    #[test]
    fn saving_a_valid_file_reports_success() {
        let (dir, path) = scratch("save-ok");
        let (mut w, core, _open) = window(path.clone());
        std::fs::write(
            &path,
            TvdockerConfig::default().to_toml().expect("serialize"),
        )
        .expect("write the isolated config");
        w.reload_editor_text_for_test();
        crate::testutil::with_ctx(|ctx| w.save(ctx));
        assert_eq!(
            core.borrow().info.as_deref(),
            Some("Settings saved"),
            "the success latch"
        );
        assert!(core.borrow().last_error.is_none(), "no error latched");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// §6 row 3: a typo in a key string is named, not dropped.
    #[test]
    fn saving_with_a_bad_key_names_the_field() {
        let (dir, path) = scratch("save-bad");
        let (mut w, core, _open) = window(path.clone());
        std::fs::write(&path, "[keybindings]\nquit = [\"banana\"]\n").expect("write the config");
        w.reload_editor_text_for_test();
        crate::testutil::with_ctx(|ctx| w.save(ctx));
        let err = core.borrow().last_error.clone().expect("an error latched");
        assert!(err.contains("keybindings.quit"), "names the field: {err}");
        assert!(err.contains("banana"), "names the value: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A "Settings saved" that implies more than was delivered misleads: an
    /// unwired field (§7.1) and a dropped surplus chord (§7.2, only the
    /// FIRST binds) must be disclosed, and the effective chord named — the
    /// bar's label still shows the default key.
    #[test]
    fn saving_discloses_what_the_overrides_actually_do() {
        let (dir, path) = scratch("save-notes");
        let (mut w, core, _open) = window(path.clone());
        let mut cfg = TvdockerConfig::default();
        cfg.keybindings.search = vec!["/".to_string()]; // unwired (§7.1)
        cfg.keybindings.quit = vec!["Ctrl-Q".into(), "Ctrl-J".into()]; // surplus
        std::fs::write(&path, cfg.to_toml().expect("serialize")).expect("write the config");
        w.reload_editor_text_for_test();
        crate::testutil::with_ctx(|ctx| w.save(ctx));
        let info = core.borrow().info.clone().expect("a status message");
        assert!(
            info.contains("keybindings.search") && info.contains("no action yet"),
            "the unwired field is disclosed: {info}"
        );
        assert!(
            info.contains("only the first chord binds"),
            "the dropped chord is disclosed: {info}"
        );
        assert!(
            info.contains("keybindings.quit → Ctrl-Q"),
            "the effective chord is named: {info}"
        );
        assert!(
            core.borrow().last_error.is_none(),
            "these are notes, not failures"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
