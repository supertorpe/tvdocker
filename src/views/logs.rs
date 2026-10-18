//! The Logs window ×N : pane (info · search line · LogView)
//! over one container's buffer in `AppCore` , with the
//! follow/pause, search, timestamps and export behaviors.

use crate::core::AppCore;
use crate::domain::docker::logs::LogLine;
use crate::widgets::pane::PaneWindow;
use crate::widgets::{FilterBar, LogView};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use tv::event::{Event, Key, MouseEvent};
use tv::menu::{Menu, popup_menu};
use tv::{Context, DrawCtx, HelpCtx, Rect, Role, View, ViewState, delegate};

/// Help context while a Logs window is focused (— binds F7).
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.logs");

/// Window-menu number for Logs instances (shared by the
/// ×N instances; Alt-\<digit\> picks the first).
const WINDOW_NUMBER: i16 = 8;

/// What the next sync must do — pure, so the tail-vs-
/// rebuild decision is unit-tested without a view.
#[derive(Debug, PartialEq, Eq)]
enum SyncPlan {
    Skip,
    /// Append `buffer[start..]` (no trim happened).
    Tail(usize),
    /// Clear + push everything (front trimmed, or a forced rebuild).
    Rebuild,
}

fn sync_plan(ingested: u64, len: usize, last: (u64, usize)) -> SyncPlan {
    if (ingested, len) == last {
        return SyncPlan::Skip;
    }
    let grew = ingested >= last.0 && len >= last.1 && (ingested - last.0) == (len - last.1) as u64;
    if grew {
        SyncPlan::Tail(last.1)
    } else {
        SyncPlan::Rebuild
    }
}

/// Live counts for the info row (updated after every sync).
struct LogsInfo {
    lines: usize,
    matches: usize,
    following: bool,
    filter: String,
    note: Option<String>,
}

/// The pane's info row: counts, follow state, the
/// `(reconnecting…)` stream state (title
/// suffix lives here), and export notes.
struct LogsInfoView {
    st: ViewState,
    core: Rc<RefCell<AppCore>>,
    id: String,
    info: Rc<RefCell<LogsInfo>>,
}

impl View for LogsInfoView {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let core = self.core.borrow();
        let info = self.info.borrow();
        let ok = core.logs.get(&self.id).is_none_or(|b| b.ok);
        let reconnect = if ok {
            String::new()
        } else {
            " (reconnecting…)".to_string()
        };
        let note = info
            .note
            .as_deref()
            .map_or(String::new(), |n| format!(" · {n}"));
        let line = if info.lines == 0 {
            format!(" Waiting for logs…{reconnect}{note}")
        } else {
            format!(
                " {} lines · {} matching · {} · search {}{reconnect}{note}",
                info.lines,
                info.matches,
                if info.following {
                    "following"
                } else {
                    "paused"
                },
                if info.filter.is_empty() {
                    "-"
                } else {
                    &info.filter
                },
            )
        };
        ctx.put_str(0, 0, &line, ctx.style(Role::Info));
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// The Logs window: `PaneWindow` wrapper whose body is a `LogView`.
pub struct LogsWindow {
    pane: PaneWindow,
    core: Rc<RefCell<AppCore>>,
    /// Buffer key — the container id.
    id: String,
    /// Search sink (shared with the FilterBar).
    sink: Rc<RefCell<String>>,
    info: Rc<RefCell<LogsInfo>>,
    /// `(ingested, len)` last applied — the incremental-sync key.
    last_sync: (u64, usize),
    show_timestamps: bool,
    /// Where the last export landed (test hook, like `table_ids`).
    last_export: Option<PathBuf>,
    /// Cached screen origin for right-click/Shift+F10 → `popup_menu` (the
    /// editor/`status_line` pattern: `abs_origin = ctx.origin()`).
    abs_origin: tv::Point,
    /// Slot open flag — cleared on drop so the window can be reopened.
    open: Rc<RefCell<bool>>,
}

/// Build a Logs window for one container (the caller — desktop or
/// test — has already created/kept the `core.logs[id]` session).
pub fn build(
    core: Rc<RefCell<AppCore>>,
    bounds: Rect,
    id: &str,
    name: &str,
    open: Rc<RefCell<bool>>,
) -> LogsWindow {
    let sink = Rc::new(RefCell::new(String::new()));
    let info = Rc::new(RefCell::new(LogsInfo {
        lines: 0,
        matches: 0,
        following: true,
        filter: String::new(),
        note: None,
    }));
    let mut pane = PaneWindow::new(
        bounds,
        format!("Logs — {name}"),
        WINDOW_NUMBER,
        Box::new(LogsInfoView {
            st: ViewState::new(Rect::new(0, 0, 1, 1)),
            core: Rc::clone(&core),
            id: id.to_string(),
            info: Rc::clone(&info),
        }),
        Box::new(FilterBar::new(
            Rect::new(0, 0, 1, 1),
            Rc::clone(&sink),
            "search",
        )),
        |rect, bar| Box::new(LogView::new(rect, bar)),
    );
    pane.state_mut().help_ctx = HELP_CTX;
    LogsWindow {
        pane,
        core,
        id: id.to_string(),
        sink,
        info,
        last_sync: (0, 0),
        show_timestamps: false,
        last_export: None,
        abs_origin: tv::Point::new(0, 0),
        open,
    }
}

impl LogsWindow {
    fn log_mut(&mut self) -> Option<&mut LogView> {
        let body = self.pane.ids().body;
        self.pane
            .window_mut()
            .child_mut(body)
            .and_then(|v| v.as_any_mut().and_then(|a| a.downcast_mut::<LogView>()))
    }

    fn filter_mut(&mut self) -> Option<&mut FilterBar> {
        let id = self.pane.ids().filter;
        self.pane
            .window_mut()
            .child_mut(id)
            .and_then(|v| v.as_any_mut().and_then(|a| a.downcast_mut::<FilterBar>()))
    }

    /// One buffer line as the view should show it (timestamps toggle).
    fn display(&self, l: &LogLine) -> String {
        if self.show_timestamps && !l.timestamp.is_empty() {
            format!("{} {}", l.timestamp, l.message)
        } else {
            l.message.clone()
        }
    }

    /// Buffer → view sync: tail-append normally, full
    /// rebuild only after a front-trim or a forced rebuild.
    fn sync_lines(&mut self) {
        let core = self.core.borrow();
        let Some(buf) = core.logs.get(&self.id) else {
            return;
        };
        let (ingested, len) = (buf.ingested, buf.lines.len());
        let plan = sync_plan(ingested, len, self.last_sync);
        let payload: Vec<LogLine> = match plan {
            SyncPlan::Skip => return,
            SyncPlan::Tail(start) => buf.lines.get(start..).unwrap_or(&[]).to_vec(),
            SyncPlan::Rebuild => buf.lines.clone(),
        };
        let rebuild = plan == SyncPlan::Rebuild;
        drop(core); // release before `&mut self`
        let display: Vec<String> = payload.iter().map(|l| self.display(l)).collect();
        if rebuild && let Some(log) = self.log_mut() {
            log.clear();
        }
        if let Some(log) = self.log_mut() {
            for l in display {
                log.push_line(l);
            }
        }
        self.last_sync = (ingested, len);
    }

    /// Push the view's live numbers into the info row.
    fn pull_state(&mut self) {
        let (lines, following) = match self.log_mut() {
            Some(l) => (l.len(), l.follows()),
            None => (0, true),
        };
        let needle = self.sink.borrow().clone();
        let matches = self.log_mut().map_or(0, |l| l.count_matching(&needle));
        let mut info = self.info.borrow_mut();
        info.lines = lines;
        info.matches = matches;
        info.following = following;
    }

    fn toggle_follow(&mut self, ctx: &mut Context) {
        if let Some(log) = self.log_mut() {
            log.set_follow(!log.follows());
            log.sync(ctx);
        }
        self.pull_state();
    }

    fn toggle_timestamps(&mut self) {
        self.show_timestamps = !self.show_timestamps;
        // Force the Rebuild path: (MAX, MAX) can never look "grew".
        self.last_sync = (u64::MAX, usize::MAX);
        self.sync_lines();
    }

    /// Context-menu action: write the DISPLAY lines to
    /// the temp dir, note the path — or say why nothing was written.
    fn export(&mut self) {
        let lines: Vec<String> = {
            let Some(log) = self.log_mut() else {
                return;
            };
            (0..log.len())
                .filter_map(|i| log.line(i).map(str::to_string))
                .collect()
        };
        if lines.is_empty() {
            self.info.borrow_mut().note = Some("No logs to export".to_string());
            return;
        }
        let short: String = self.id.chars().take(12).collect();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let path = std::env::temp_dir().join(format!("tvdocker_logs_{short}_{stamp}.log"));
        match std::fs::write(&path, lines.join("\n")) {
            Ok(()) => {
                self.info.borrow_mut().note = Some(format!(
                    "Exported {} lines to {}",
                    lines.len(),
                    path.display()
                ));
                self.last_export = Some(path);
            }
            Err(e) => {
                self.info.borrow_mut().note = Some(format!("Export failed: {e}"));
            }
        }
    }

    fn right_click_menu(&mut self, m: MouseEvent, ctx: &mut Context) {
        self.context_menu_at(m.position, ctx);
    }

    /// The logs context menu (timestamps / export) at window-local
    /// `anchor` — the right-click point, or the log pane's top-left for
    /// the keyboard route (a log line has no row menu of its own).
    fn context_menu_at(&mut self, anchor: tv::Point, ctx: &mut Context) {
        let menu = Menu::builder()
            .command(
                if self.show_timestamps {
                    "Hide timestamps"
                } else {
                    "Show timestamps"
                },
                crate::app::CMD_TOGGLE_TIMESTAMPS,
            )
            .command("Export…", crate::app::CMD_EXPORT_LOGS)
            .build();
        let global = anchor + self.abs_origin;
        popup_menu(global, menu, ctx.owner_size(), ctx);
    }
}

#[delegate(to = pane)]
impl View for LogsWindow {
    fn state(&self) -> &ViewState {
        self.pane.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.pane.state_mut()
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.abs_origin = ctx.origin(); // right-click → global coords
        self.pane.draw(ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // Esc priority chain, search rung: clear the search first, close second.
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::Esc
        {
            if !self.sink.borrow().is_empty() {
                if let Some(fb) = self.filter_mut() {
                    fb.clear();
                } else {
                    self.sink.borrow_mut().clear();
                }
                if let Some(log) = self.log_mut() {
                    log.set_highlight(None);
                }
                self.info.borrow_mut().filter = String::new();
                self.pull_state();
                ev.clear();
                return;
            }
            if let Some(id) = self.state().id() {
                ctx.request_close(id);
                ev.clear();
            }
            return;
        }
        // --- F7 : both the pre-route's Command and a direct
        // KeyDown end in the same toggle.
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::F(7)
        {
            self.toggle_follow(ctx);
            ev.clear();
            return;
        }
        if let Event::Command(cmd) = ev {
            if *cmd == crate::app::CMD_TOGGLE_FOLLOW {
                self.toggle_follow(ctx);
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_TOGGLE_TIMESTAMPS {
                self.toggle_timestamps();
                self.pull_state();
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_EXPORT_LOGS {
                self.export();
                ev.clear();
                return;
            }
        }
        // --- Right-click: timestamps/export live in the context menu
        // (— printable keys would be eaten by the search line).
        if let Event::MouseDown(m) = ev {
            let m = *m;
            if m.buttons.right && !m.buttons.left {
                self.right_click_menu(m, ctx);
                ev.clear();
                return;
            }
        }
        // --- Shift+F10: the mouse-free route to the same menu — anchored
        // at the log pane's top-left (a log has no row to anchor on).
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::F(10)
            && ke.modifiers.shift
        {
            let body = self.pane.ids().body;
            let anchor = self
                .pane
                .window_mut()
                .child_mut(body)
                .map(|v| v.state().get_bounds().a)
                .unwrap_or(tv::Point::new(0, 0));
            self.context_menu_at(anchor, ctx);
            ev.clear();
            return;
        }

        self.pane.handle_event(ev, ctx);

        // Search sink → highlight (the probe's after_event pattern).
        let needle = self.sink.borrow().clone();
        if needle != self.info.borrow().filter {
            if let Some(log) = self.log_mut() {
                log.set_highlight(if needle.is_empty() {
                    None
                } else {
                    Some(needle.clone())
                });
            }
            self.info.borrow_mut().filter = needle;
        }
        self.sync_lines();
        if let Some(log) = self.log_mut() {
            log.sync(ctx); // follow tail / publish limits
        }
        self.pull_state();
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Drop for LogsWindow {
    /// Tell the worker's session to stop (the flag was created when
    /// the desktop opened the session), and clear the
    /// slot's open flag so the window can be reopened.
    fn drop(&mut self) {
        if let Some(buf) = self.core.borrow().logs.get(&self.id) {
            buf.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        *self.open.borrow_mut() = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AppMsg, LOGS_CAP};

    fn line(ts: &str, msg: &str) -> LogLine {
        LogLine {
            timestamp: ts.to_string(),
            message: msg.to_string(),
        }
    }

    fn session_core(id: &str) -> Rc<RefCell<AppCore>> {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut().set_logs_session(id);
        core
    }

    fn window(core: Rc<RefCell<AppCore>>) -> LogsWindow {
        build(
            core,
            Rect::new(2, 1, 78, 23),
            "c1",
            "web",
            Rc::new(RefCell::new(true)),
        )
    }

    fn apply_lines(core: &Rc<RefCell<AppCore>>, id: &str, lines: Vec<LogLine>) {
        core.borrow_mut().apply(AppMsg::LogLines {
            id: id.to_string(),
            lines,
        });
    }

    #[test]
    fn sync_plan_picks_skip_tail_or_rebuild() {
        use SyncPlan::*;
        // Same state ⇒ nothing to do.
        assert!(matches!(sync_plan(10, 10, (10, 10)), Skip));
        // Normal batch: +k ingested, +k len ⇒ append the tail from len.
        assert!(matches!(sync_plan(15, 15, (10, 10)), Tail(10)));
        // Front trim: ingested grew MORE than len ⇒ full rebuild.
        assert!(matches!(sync_plan(1_015, 1_000, (10, 10)), Rebuild));
        // Length shrank (session reset by reopen) ⇒ rebuild.
        assert!(matches!(sync_plan(2, 0, (10, 10)), Rebuild));
        // The forced rebuild marker after a timestamp toggle.
        assert!(matches!(sync_plan(10, 10, (u64::MAX, usize::MAX)), Rebuild));
    }

    #[test]
    fn log_sync_is_incremental_and_survives_a_cap_trim() {
        let core = session_core("c1");
        let mut w = window(core.clone());
        apply_lines(
            &core,
            "c1",
            (0..10)
                .map(|i| line("2026-10-08T10:00:00Z", &format!("m{i}")))
                .collect(),
        );
        w.sync_lines();
        w.pull_state();
        assert_eq!(w.log_mut().expect("view").len(), 10);
        assert_eq!(w.info.borrow().lines, 10);

        apply_lines(
            &core,
            "c1",
            (10..15)
                .map(|i| line("2026-10-08T10:00:01Z", &format!("m{i}")))
                .collect(),
        );
        w.sync_lines();
        let view = w.log_mut().expect("view");
        assert_eq!(view.len(), 15, "tail appended");

        // Cross the cap: the CORE trims the front ⇒ the window takes the
        // rebuild path and ends byte-for-byte identical to the buffer.
        let overflow = LOGS_CAP + 5 - 15;
        apply_lines(
            &core,
            "c1",
            (15..15 + overflow as u64)
                .map(|i| line("2026-10-08T10:00:02Z", &format!("m{i}")))
                .collect(),
        );
        w.sync_lines();
        let view = w.log_mut().expect("view");
        assert_eq!(view.len(), LOGS_CAP, "rebuild honours the cap");
        assert_eq!(
            view.line(0),
            core.borrow().logs["c1"]
                .lines
                .first()
                .map(|l| l.message.as_str()),
            "first line equals the buffer's first (front was trimmed)"
        );
    }

    #[test]
    fn timestamps_toggle_rebuilds_with_the_prefix() {
        let core = session_core("c1");
        let mut w = window(core.clone());
        apply_lines(&core, "c1", vec![line("2026-10-08T10:00:00Z", "hello")]);
        w.sync_lines();
        assert_eq!(w.log_mut().expect("view").line(0), Some("hello"));

        crate::testutil::send_event(&mut w, Event::Command(crate::app::CMD_TOGGLE_TIMESTAMPS));
        assert_eq!(
            w.log_mut().expect("view").line(0),
            Some("2026-10-08T10:00:00Z hello"),
            "timestamp prefixed on toggle"
        );

        crate::testutil::send_event(&mut w, Event::Command(crate::app::CMD_TOGGLE_TIMESTAMPS));
        assert_eq!(
            w.log_mut().expect("view").line(0),
            Some("hello"),
            "toggle back strips the prefix again"
        );
    }

    #[test]
    fn f7_and_its_command_both_toggle_follow() {
        let core = session_core("c1");
        let mut w = window(core);
        assert!(
            w.log_mut().expect("view").follows(),
            "sessions start following"
        );
        crate::testutil::send_event(&mut w, Event::Command(crate::app::CMD_TOGGLE_FOLLOW));
        assert!(!w.log_mut().expect("view").follows(), "paused");
        assert!(!w.info.borrow().following, "info row mirrors it");
        crate::testutil::send_key(&mut w, tv::event::Key::F(7));
        assert!(w.log_mut().expect("view").follows(), "the KeyDown arm too");
    }

    #[test]
    fn export_writes_exactly_the_buffered_lines() {
        let core = session_core("c1");
        let mut w = window(core.clone());
        apply_lines(
            &core,
            "c1",
            vec![line("", "one"), line("", "two"), line("", "three")],
        );
        w.sync_lines();
        crate::testutil::send_event(&mut w, Event::Command(crate::app::CMD_EXPORT_LOGS));
        let path = w.last_export.clone().expect("export produced a path");
        let content = std::fs::read_to_string(&path).expect("readable");
        assert_eq!(
            content.lines().count(),
            3,
            "N = buffer count (deferred row)"
        );
        assert!(!content.contains('\u{1b}'), "export is display text");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exporting_an_empty_window_notes_instead_of_writing() {
        let core = session_core("c1");
        let mut w = window(core);
        crate::testutil::send_event(&mut w, Event::Command(crate::app::CMD_EXPORT_LOGS));
        assert!(w.last_export.is_none(), "nothing written");
        assert_eq!(
            w.info.borrow().note.as_deref(),
            Some("No logs to export"),
            "the window says why"
        );
    }

    #[test]
    fn reconnect_state_shows_in_the_info_row() {
        let core = session_core("c1");
        let mut w = window(core.clone());
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(out.contains("Waiting for logs"), "empty state: {out:?}");
        assert!(!out.contains("reconnecting"), "healthy at first: {out:?}");

        core.borrow_mut().apply(AppMsg::LogStatus {
            id: "c1".into(),
            ok: false,
        });
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(
            out.contains("(reconnecting…)"),
            "spec-§9 state in the info row: {out:?}"
        );
    }

    #[test]
    fn esc_clears_the_search_rung_without_touching_the_window() {
        let core = session_core("c1");
        let mut w = window(core);
        w.sink.borrow_mut().push_str("err"); // a search is active
        crate::testutil::send_event(
            &mut w,
            Event::KeyDown(tv::event::KeyEvent::new(
                Key::Esc,
                tv::event::KeyModifiers::default(),
            )),
        );
        assert!(w.sink.borrow().is_empty(), "search cleared first");
        assert_eq!(w.info.borrow().filter, "", "counts follow");
        assert!(w.info.borrow().note.is_none(), "window untouched");
        // A SECOND Esc is the close rung — proven end-to-end in
        // tests/logs.rs (request_close needs the real pipeline).
    }
}
