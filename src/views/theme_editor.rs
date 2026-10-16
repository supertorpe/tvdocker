//! Options ▸ Edit colors… : author a palette over a WORKING copy.
//!
//! tvision ships `Program::theme_editor()`, but it exposes no way to read the
//! edited theme back (`Program` keeps `theme` private and only `set_theme`
//! writes it) — its output could never be persisted to `tvdocker.toml`. So the
//! editor is ours: a role list (with a scroll bar) over a `Theme` we own,
//! `f`/`b` open tvision's friendly [`ColorPicker`] inside [`ColorPickDialog`],
//! and `run_app` applies the working copy on OK and then offers "save as
//! theme".
//!
//! # The nested dialog must fit inside the parent
//!
//! tvision gates positional events through EVERY modal frame on the capture
//! stack (`CaptureStack::dispatch` walks it back-to-front and a gate
//! *consumes* anything outside its own bounds). A nested modal that sticks out
//! of its parent is therefore mouse-dead wherever it overhangs — which is
//! exactly how the colour dialog's OK/Cancel row lost its clicks. [`Self::begin_color`]
//! therefore centers the nested dialog inside the editor's own bounds.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::domain::theme::{ALL_ROLES, color_hex};
use tv::dialog::{ButtonRowAlign, ColorPicker, Dialog};
use tv::event::{Event, Key, MouseWheel};
use tv::widgets::{ButtonFlags, ScrollBar};
use tv::{Color, Command, Context, DrawCtx, Rect, Role, Theme, View, ViewId, ViewState};

/// Posted by the nested colour dialog to the editor (the
/// `request_exec_view(.., then_command)` seam) — distinct from `Command::OK`,
/// which commits the editor itself. Fires on OK *and* cancel; the sink tells
/// the two apart.
pub const CMD_THEME_COLOR_SET: Command = Command::custom("tvdocker.theme_color_set");

/// Fixed dialog geometry (frame included): header + role list + hint, with the
/// OK/Cancel row on the last content rows (`button_row`'s `height − 3`).
const DIALOG_W: i32 = 66;
/// Tall enough to host the nested colour dialog inside the editor's bounds on
/// a standard 25-row terminal (the picker wants 62×23; the editor must be at
/// least that tall or the parent's modal gate swallows the picker's buttons).
const DIALOG_H: i32 = 21;
/// Role rows drawn (80 roles ⇒ the list scrolls with the scroll bar).
const LIST_ROWS: usize = 14;
/// Colour the nested input dialog is editing: `(role, editing_foreground)`.
type Pending = Option<(Role, bool)>;

/// Centered `Rect` for the editor inside `desktop`.
pub fn editor_rect(desktop: Rect) -> Rect {
    let size = desktop.b - desktop.a;
    let w = DIALOG_W.min(size.x.max(6));
    let h = DIALOG_H.min(size.y.max(6));
    let x = desktop.a.x + (size.x - w) / 2;
    let y = desktop.a.y + (size.y - h) / 2;
    Rect::new(x, y, x + w, y + h)
}

pub struct ThemeEditorDialog {
    dialog: Dialog,
    /// The working palette — the rows view reads it, `f`/`b` mutate it.
    theme: Rc<RefCell<Theme>>,
    cursor: Rc<Cell<usize>>,
    pending: Rc<Cell<Pending>>,
    /// What the nested [`ColorPickDialog`] chose: `None` until it reports OK
    /// (the shared-sink round trip `InputDialog` and the explorer prompts use).
    sink: Rc<RefCell<Option<Color>>>,
    /// The role list's vertical scroll bar.
    bar_id: ViewId,
}

impl ThemeEditorDialog {
    pub fn new(bounds: Rect, theme: Theme) -> Self {
        let mut dialog = Dialog::new(bounds, Some("Edit colors".to_string()));
        let w = bounds.b.x - bounds.a.x;
        let h = bounds.b.y - bounds.a.y;
        dialog.button_row(
            &[
                (
                    "~O~K",
                    Command::OK,
                    ButtonFlags {
                        default: true,
                        ..Default::default()
                    },
                ),
                ("~C~ancel", Command::CANCEL, ButtonFlags::default()),
            ],
            ButtonRowAlign::Right,
        );
        // Vertical bar down the right inner edge, one column clear of the
        // frame, spanning the role rows (+1 so the arrows sit beside them).
        let mut bar = ScrollBar::new(Rect::new(w - 3, 1, w - 2, LIST_ROWS as i32 + 2));
        bar.min_value = 0;
        bar.max_value = (ALL_ROLES.len() - 1) as i32;
        bar.value = 0;
        bar.page_step = (LIST_ROWS - 1) as i32;
        bar.arrow_step = 1;
        let bar_id = dialog.insert_child(Box::new(bar));
        let theme = Rc::new(RefCell::new(theme));
        let cursor = Rc::new(Cell::new(0));
        let pending: Rc<Cell<Pending>> = Rc::new(Cell::new(None));
        let sink = Rc::new(RefCell::new(None));
        // LAST: a modal with no explicit focus selects the last inserted
        // selectable child, so the list takes the keys.
        let rows = ThemeRowsView {
            // Left of the bar, above the button row — an overlap would win the
            // hit-test over both (children are searched bottom-up).
            st: ViewState::new(Rect::new(1, 1, w - 3, h - 3)),
            theme: Rc::clone(&theme),
            cursor: Rc::clone(&cursor),
        };
        dialog.insert_child(Box::new(rows));
        Self {
            dialog,
            theme,
            cursor,
            pending,
            sink,
            bar_id,
        }
    }

    /// The palette as edited — what `run_app` installs on OK.
    pub fn theme(&self) -> Theme {
        self.theme.borrow().clone()
    }

    /// Move the role cursor (the bar is published once, after dispatch —
    /// `handle_event` mirrors any cursor change, including one made by the
    /// rows view's own mouse handling).
    fn move_cursor(&mut self, want: usize) {
        self.cursor.set(want.min(ALL_ROLES.len() - 1));
    }

    /// Mirror the cursor into the scroll bar (no-op when it did not move).
    fn publish_bar(&mut self, ctx: &mut Context) {
        if let Some(bar) = self
            .dialog
            .child_mut(self.bar_id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<ScrollBar>())
        {
            bar.set_value(self.cursor.get() as i32, ctx);
        }
    }

    /// Open tvision's colour picker for the cursor row's fg (`true`) or bg,
    /// seeded with the colour currently in the working copy.
    ///
    /// The dialog is sized and centered INSIDE the editor's own bounds: every
    /// modal frame on the capture stack gates positional events by its own
    /// bounds, so a nested modal that overhangs its parent loses its mouse
    /// (see the module doc).
    ///
    /// The pending slot and the sink are reset BEFORE the modal request so
    /// the round trip is observable even without a `ViewId` (unit tests run
    /// the dialog uninserted).
    fn begin_color(&mut self, fg: bool, ctx: &mut Context) {
        let role = ALL_ROLES[self.cursor.get().min(ALL_ROLES.len() - 1)];
        let style = self.theme.borrow().style(role);
        let initial = if fg { style.fg } else { style.bg };
        self.pending.set(Some((role, fg)));
        *self.sink.borrow_mut() = None;
        let Some(requester) = self.dialog.state().id() else {
            return;
        };
        let mine = self.dialog.state().get_bounds();
        let (avail_w, avail_h) = (mine.b.x - mine.a.x, mine.b.y - mine.a.y);
        let (w, h) = (62.min(avail_w), 23.min(avail_h));
        let x = mine.a.x + (avail_w - w) / 2;
        let y = mine.a.y + (avail_h - h) / 2;
        ctx.request_exec_view(
            Box::new(ColorPickDialog::new(
                Rect::new(x, y, x + w, y + h),
                initial,
                Rc::clone(&self.sink),
            )),
            requester,
            Some(CMD_THEME_COLOR_SET),
        );
    }

    /// Fold the picker's answer into the pending slot; an empty sink means
    /// the user cancelled, which clears the slot and changes nothing.
    fn apply_pending(&mut self) {
        let Some((role, fg)) = self.pending.take() else {
            return;
        };
        let Some(color) = self.sink.borrow_mut().take() else {
            return;
        };
        let mut theme = self.theme.borrow_mut();
        let mut style = theme.style(role);
        if fg {
            style.fg = color;
        } else {
            style.bg = color;
        }
        theme.set_style(role, style);
    }

    /// The routing body; the cursor may move here — by key, or by a
    /// click/wheel the rows view handles.
    fn dispatch(&mut self, ev: &mut Event, ctx: &mut Context) {
        if let Event::Broadcast { command, source } = *ev {
            let own = source.is_some() && source == Some(self.bar_id);
            if own
                && command == Command::SCROLL_BAR_CHANGED
                && let Some(id) = self.dialog.state().id()
            {
                ctx.request_scroll_sync(id, None, Some(self.bar_id));
            } else if own
                && command == Command::SCROLL_BAR_CLICKED
                && let Some(id) = self.dialog.state().id()
            {
                ctx.request_focus(id);
            }
            return;
        }
        if let Event::KeyDown(ke) = ev {
            let last = ALL_ROLES.len() - 1;
            let cur = self.cursor.get().min(last);
            match ke.key {
                Key::Down => self.move_cursor((cur + 1).min(last)),
                Key::Up => self.move_cursor(cur.saturating_sub(1)),
                Key::PageDown => self.move_cursor((cur + LIST_ROWS - 1).min(last)),
                Key::PageUp => self.move_cursor(cur.saturating_sub(LIST_ROWS - 1)),
                Key::Home => self.move_cursor(0),
                Key::End => self.move_cursor(last),
                Key::Char('f') | Key::Char('F') => {
                    self.begin_color(true, ctx);
                    ev.clear();
                    return;
                }
                Key::Char('b') | Key::Char('B') => {
                    self.begin_color(false, ctx);
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
                _ => {
                    self.dialog.handle_event(ev, ctx);
                    return;
                }
            }
            ev.clear();
            return;
        }
        if let Event::Command(cmd) = ev {
            match *cmd {
                // The nested colour dialog reports back through the
                // `request_exec_view(.., then_command)` seam (posted, not a
                // broadcast).
                CMD_THEME_COLOR_SET => {
                    self.apply_pending();
                    ev.clear();
                    return;
                }
                Command::OK => {
                    ctx.end_modal(Command::OK);
                    ev.clear();
                    return;
                }
                Command::CANCEL => {
                    ctx.end_modal(Command::CANCEL);
                    ev.clear();
                    return;
                }
                _ => {}
            }
        }
        self.dialog.handle_event(ev, ctx);
    }
}

impl View for ThemeEditorDialog {
    fn state(&self) -> &ViewState {
        self.dialog.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.dialog.state_mut()
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.dialog.draw(ctx);
    }

    /// Bar → content: the embedded list has no Scroller, so this inserted
    /// composite brokers the sync (the ListViewer pattern).
    fn apply_scroll_sync(&mut self, _h: Option<i32>, v: Option<i32>, _ctx: &mut Context) {
        if let Some(v) = v {
            self.cursor
                .set((v.max(0) as usize).min(ALL_ROLES.len() - 1));
        }
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        let before = self.cursor.get();
        self.dispatch(ev, ctx);
        if self.cursor.get() != before {
            // A click/wheel in the rows view moved the cursor behind our
            // back — mirror it into the scroll bar (keys go through
            // `move_cursor` + this same publication).
            self.publish_bar(ctx);
        }
    }

    /// The pump reaches INTO this wrapper by id — the scroll-sync broker
    /// (`request_scroll_sync` targets this dialog) and any tracked mouse
    /// delivery resolve through `find_mut`, whose base returns `None`.
    fn find_mut(&mut self, id: ViewId) -> Option<&mut dyn View> {
        self.dialog.find_mut(id)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// tvision's [`ColorPicker`] body inside dialog chrome, reading the chosen
/// colour into a shared sink on OK — the "friendly" way to set a colour
/// (Presets / RGB / Plane / Xterm-256 tabs, swatches, live info column).
///
/// `Program::color_dialog` does the same Program-side with a gather closure
/// over its own modal; a View cannot use that seam (there is no
/// [`tv::data::FieldValue`] for a colour and gather ids only exist on
/// `exec_view_capture`), so this wrapper uses the repo's shared-sink round
/// trip — the same one `InputDialog` and the explorer prompts rely on. The
/// colour is read on `Command::OK` *before* the dialog tears the modal down.
pub struct ColorPickDialog {
    dialog: Dialog,
    picker_id: ViewId,
    sink: Rc<RefCell<Option<Color>>>,
}

impl ColorPickDialog {
    /// The widget-gallery's `colorpicker` specimen layout (see the tvision-rs
    /// guide): a 62×23 dialog with the picker on rows 2..18 — it needs at
    /// least a 56×18 content area — a blank margin row, then the OK/Cancel
    /// row at `height − 3`, right-grouped.
    ///
    /// One deliberate difference from the specimen: the buttons go in FIRST
    /// and the picker LAST. The specimen lives on a desktop and can name its
    /// focus (`exec_view_capture`'s `initial_focus`); a modal opened through
    /// `request_exec_view` cannot, and falls back to the LAST inserted
    /// selectable child — so the picker takes the focus, exactly what
    /// `Program::color_dialog` arranges.
    pub fn new(bounds: Rect, initial: Color, sink: Rc<RefCell<Option<Color>>>) -> Self {
        let mut dialog = Dialog::new(bounds, Some("Select Color".to_string()));
        let w = bounds.b.x - bounds.a.x;
        let h = bounds.b.y - bounds.a.y;
        dialog.button_row(
            &[
                (
                    "~O~K",
                    Command::OK,
                    ButtonFlags {
                        default: true,
                        ..Default::default()
                    },
                ),
                ("~C~ancel", Command::CANCEL, ButtonFlags::default()),
            ],
            ButtonRowAlign::Right,
        );
        let picker_id = dialog.insert_child(Box::new(ColorPicker::new(
            Rect::new(2, 2, w - 2, h - 4),
            initial,
        )));
        Self {
            dialog,
            picker_id,
            sink,
        }
    }

    /// Read the picker's current selection into the sink while the modal is
    /// still in the tree (the same moment `color_dialog` gathers).
    fn capture(&mut self) {
        let color = self
            .dialog
            .find_mut(self.picker_id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<ColorPicker>())
            .map(|p| p.color());
        *self.sink.borrow_mut() = color;
    }
}

impl View for ColorPickDialog {
    fn state(&self) -> &ViewState {
        self.dialog.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.dialog.state_mut()
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.dialog.draw(ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // Capture FIRST: routing `Command::OK` into the Dialog ends the modal
        // (and drops the picker) before anything else could read it.
        if matches!(*ev, Event::Command(Command::OK)) {
            self.capture();
        }
        self.dialog.handle_event(ev, ctx);
    }

    /// The pump reaches INTO this wrapper by id (a tracked MouseUp from the
    /// buttons' `MouseTrackCapture`, a `ScrollSync` …); the base `find_mut`
    /// returns `None`, so those deliveries would fall out of the tree.
    fn find_mut(&mut self, id: ViewId) -> Option<&mut dyn View> {
        self.dialog.find_mut(id)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// The header, the scrolling role rows (with a chip in each role's own
/// colours) and the key hint; draw-only — the wrapper owns the keys.
struct ThemeRowsView {
    st: ViewState,
    theme: Rc<RefCell<Theme>>,
    cursor: Rc<Cell<usize>>,
}

impl View for ThemeRowsView {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    /// Click or wheel: the group routes positional events to the child under
    /// the cursor, so `y` arrives already child-local (the dialog subtracts
    /// this view's origin before delivery).
    fn handle_event(&mut self, ev: &mut Event, _ctx: &mut Context) {
        match ev {
            Event::MouseDown(m) if m.buttons.left => {
                // Row `0` is the header; the list runs 1..=LIST_ROWS and is
                // drawn as a window over the roles.
                let y = m.position.y;
                if (1..=LIST_ROWS as i32).contains(&y) {
                    let first = self.cursor.get().saturating_sub(LIST_ROWS - 1);
                    let idx = first + (y - 1) as usize;
                    if idx < ALL_ROLES.len() {
                        self.cursor.set(idx);
                        ev.clear();
                    }
                }
            }
            // The wheel scrolls the list; the wrapper publishes the new
            // cursor to the scroll bar afterwards.
            Event::MouseWheel(m) => {
                let delta = match m.wheel {
                    MouseWheel::Up => -3,
                    MouseWheel::Down => 3,
                    _ => 0,
                };
                if delta != 0 {
                    let last = (ALL_ROLES.len() - 1) as i32;
                    let next = (self.cursor.get() as i32 + delta).clamp(0, last);
                    self.cursor.set(next as usize);
                    ev.clear();
                }
            }
            _ => {}
        }
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        let theme = self.theme.borrow();
        let last = ALL_ROLES.len() - 1;
        let cur = self.cursor.get().min(last);
        let header_style = ctx.style(Role::ListSurface);
        ctx.put_str(
            1,
            0,
            &format!("  {:<19} {:<8} {:<8}", "role", "fg", "bg"),
            header_style,
        );
        let first = cur.saturating_sub(LIST_ROWS - 1);
        for (row, i) in (first..=last.min(first + LIST_ROWS - 1)).enumerate() {
            let role = ALL_ROLES[i];
            let style = theme.style(role);
            let mark = if i == cur { "»" } else { " " };
            let y = 1 + row as i32;
            let line_style = if i == cur {
                ctx.style(Role::ListFocused)
            } else {
                ctx.style(Role::ListNormal)
            };
            ctx.put_str(
                1,
                y,
                &format!(
                    "{mark} {:<19} {:<8} {:<8}",
                    role.name(),
                    color_hex(style.fg),
                    color_hex(style.bg)
                ),
                line_style,
            );
            // The chip IS the preview: this role's own foreground/background.
            ctx.put_str(44, y, "█████", style);
        }
        ctx.put_str(
            1,
            LIST_ROWS as i32 + 2,
            "f fg · b bg · Enter apply · Esc cancel",
            ctx.style(Role::Info),
        );
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::theme;
    use std::collections::VecDeque;
    use tv::{Deferred, KeyEvent, TimerQueue};

    fn editor() -> ThemeEditorDialog {
        ThemeEditorDialog::new(
            Rect::new(0, 0, DIALOG_W, DIALOG_H),
            theme::resolve(&Default::default(), "dark"),
        )
    }

    fn drive(dlg: &mut ThemeEditorDialog, ev: Event) -> (Vec<Deferred>, VecDeque<Event>) {
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut timers = TimerQueue::new();
        let mut deferred: Vec<Deferred> = Vec::new();
        {
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = ev;
            dlg.handle_event(&mut ev, &mut ctx);
        }
        (deferred, out)
    }

    fn key(dlg: &mut ThemeEditorDialog, key: Key) -> Vec<Deferred> {
        drive(dlg, Event::KeyDown(KeyEvent::new(key, Default::default()))).0
    }

    fn mouse_down_at(x: i32, y: i32) -> Event {
        Event::MouseDown(tv::MouseEvent {
            position: tv::Point::new(x, y),
            buttons: tv::MouseButtons {
                left: true,
                ..tv::MouseButtons::default()
            },
            ..tv::MouseEvent::default()
        })
    }

    fn wheel(dir: MouseWheel) -> Event {
        Event::MouseWheel(tv::MouseEvent {
            wheel: dir,
            ..tv::MouseEvent::default()
        })
    }

    #[test]
    fn cursor_walks_and_jumps_the_eighty_roles() {
        let mut dlg = editor();
        key(&mut dlg, Key::Down);
        assert_eq!(dlg.cursor.get(), 1);
        key(&mut dlg, Key::End);
        assert_eq!(dlg.cursor.get(), ALL_ROLES.len() - 1, "End = last role");
        key(&mut dlg, Key::Home);
        assert_eq!(dlg.cursor.get(), 0, "Home = first role");
        key(&mut dlg, Key::PageDown);
        assert_eq!(dlg.cursor.get(), LIST_ROWS - 1, "page = one screen");
        key(&mut dlg, Key::PageUp);
        assert_eq!(dlg.cursor.get(), 0, "page up clamps at the top");
    }

    #[test]
    fn moving_the_cursor_publishes_it_to_the_bar_and_the_bar_drives_it_back() {
        let mut dlg = editor();
        let (_, out) = drive(
            &mut dlg,
            Event::KeyDown(KeyEvent::new(Key::Down, Default::default())),
        );
        assert_eq!(dlg.cursor.get(), 1);
        assert!(
            out.iter().any(|e| matches!(
                e,
                Event::Broadcast { command, source: Some(s) }
                    if *command == Command::SCROLL_BAR_CHANGED && *s == dlg.bar_id
            )),
            "the bar reports the new position"
        );

        // Bar → cursor: the pump feeds the value back through the
        // scroll-sync broker (what the Broadcast arm requests).
        crate::testutil::with_ctx(|ctx| dlg.apply_scroll_sync(None, Some(5), ctx));
        assert_eq!(dlg.cursor.get(), 5, "the bar drives the cursor");
        // …and a value past the end clamps.
        crate::testutil::with_ctx(|ctx| dlg.apply_scroll_sync(None, Some(999), ctx));
        assert_eq!(dlg.cursor.get(), ALL_ROLES.len() - 1);
    }

    #[test]
    fn clicking_a_role_row_selects_it_and_the_bar_follows() {
        let mut dlg = editor();
        // The rows view origin is (1,1), so dialog-local (5, 3) arrives
        // child-local (4, 2): visible row 2-1 = 1 (row 0 is the header).
        let (_, out) = drive(&mut dlg, mouse_down_at(5, 3));
        assert_eq!(dlg.cursor.get(), 1, "the click selects the second role");
        assert!(
            out.iter().any(|e| matches!(
                e,
                Event::Broadcast { command, source: Some(s) }
                    if *command == Command::SCROLL_BAR_CHANGED && *s == dlg.bar_id
            )),
            "the bar follows a mouse selection too"
        );

        // The hint row (child-local y = LIST_ROWS + 2) is not a role.
        let _ = drive(&mut dlg, mouse_down_at(5, 1 + LIST_ROWS as i32 + 2));
        assert_eq!(dlg.cursor.get(), 1, "a click on the hint changes nothing");

        // The wheel scrolls the list (the wrapper republishes the bar).
        let _ = drive(&mut dlg, wheel(MouseWheel::Down));
        assert_eq!(dlg.cursor.get(), 4, "wheel down = 3 roles");
        let (_, out) = drive(&mut dlg, wheel(MouseWheel::Up));
        assert_eq!(dlg.cursor.get(), 1, "wheel up = 3 roles back");
        assert!(
            out.iter().any(|e| matches!(e, Event::Broadcast { .. })),
            "the bar tracks the wheel"
        );
    }

    #[test]
    fn f_arms_a_slot_and_arms_it_again() {
        let mut dlg = editor();
        key(&mut dlg, Key::Home);
        key(&mut dlg, Key::Char('f'));
        assert_eq!(
            dlg.pending.get(),
            Some((ALL_ROLES[0], true)),
            "f arms the foreground slot"
        );
        assert_eq!(*dlg.sink.borrow(), None, "the sink starts empty");
        key(&mut dlg, Key::Char('b'));
        assert_eq!(
            dlg.pending.get(),
            Some((ALL_ROLES[0], false)),
            "b arms the background slot"
        );
    }

    #[test]
    fn a_reported_colour_lands_in_the_palette_and_a_cancel_changes_nothing() {
        let mut dlg = editor();
        key(&mut dlg, Key::Home);
        key(&mut dlg, Key::Char('f'));
        *dlg.sink.borrow_mut() = Some(Color::Rgb(0x12, 0x34, 0x56));
        drive(&mut dlg, Event::Command(CMD_THEME_COLOR_SET));
        assert_eq!(
            dlg.theme().style(ALL_ROLES[0]).fg,
            Color::Rgb(0x12, 0x34, 0x56),
            "the picked colour lands in the working palette"
        );
        assert_eq!(dlg.pending.get(), None, "slot cleared");

        // Cancel: the sink stays empty, so nothing is written — and no
        // message box is queued either.
        key(&mut dlg, Key::Char('b'));
        let before = dlg.theme().style(ALL_ROLES[0]).bg;
        let (deferred, _) = drive(&mut dlg, Event::Command(CMD_THEME_COLOR_SET));
        assert_eq!(
            dlg.theme().style(ALL_ROLES[0]).bg,
            before,
            "cancel is a no-op"
        );
        assert_eq!(dlg.pending.get(), None, "slot still cleared");
        assert!(
            !deferred
                .iter()
                .any(|d| matches!(d, Deferred::OpenMessageBox { .. })),
            "a cancel is not an error: {} ops",
            deferred.len()
        );
    }

    #[test]
    fn enter_commits_the_working_palette_and_esc_discards_it() {
        let mut dlg = editor();
        key(&mut dlg, Key::Home);
        key(&mut dlg, Key::Char('f'));
        *dlg.sink.borrow_mut() = Some(Color::Default);
        drive(&mut dlg, Event::Command(CMD_THEME_COLOR_SET));
        let edited = key(&mut dlg, Key::Enter);
        assert!(
            edited
                .iter()
                .any(|d| matches!(d, Deferred::EndModal(c) if *c == Command::OK)),
            "Enter → EndModal(OK)"
        );
        assert_eq!(
            dlg.theme().style(ALL_ROLES[0]).fg,
            Color::Default,
            "the edit rides out with the palette"
        );

        let mut dlg = editor();
        key(&mut dlg, Key::End);
        let deferred = key(&mut dlg, Key::Esc);
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::EndModal(c) if *c == Command::CANCEL)),
            "Esc → EndModal(CANCEL)"
        );
    }

    fn drive_color(dlg: &mut ColorPickDialog, ev: Event) -> Vec<Deferred> {
        let mut out: VecDeque<Event> = VecDeque::new();
        let mut timers = TimerQueue::new();
        let mut deferred: Vec<Deferred> = Vec::new();
        {
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = ev;
            dlg.handle_event(&mut ev, &mut ctx);
        }
        deferred
    }

    #[test]
    fn color_pick_dialog_reads_the_picker_on_ok_and_stays_silent_on_cancel() {
        let sink = Rc::new(RefCell::new(None));
        let mut dlg = ColorPickDialog::new(
            Rect::new(0, 0, 62, 23),
            Color::Rgb(10, 20, 30),
            Rc::clone(&sink),
        );
        // A modal only ends from Dialog's command arm when the modal bit is
        // set — the exec path sets it (step 4 of `exec_view_capture`).
        dlg.state_mut().state.modal = true;

        let deferred = drive_color(&mut dlg, Event::Command(Command::OK));
        assert_eq!(
            *sink.borrow(),
            Some(Color::Rgb(10, 20, 30)),
            "OK captures the picker's current selection"
        );
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::EndModal(c) if *c == Command::OK)),
            "the Dialog ends the modal with OK"
        );

        let sink2 = Rc::new(RefCell::new(None));
        let mut dlg2 = ColorPickDialog::new(
            Rect::new(0, 0, 62, 23),
            Color::Rgb(1, 2, 3),
            Rc::clone(&sink2),
        );
        dlg2.state_mut().state.modal = true;
        let deferred = drive_color(&mut dlg2, Event::Command(Command::CANCEL));
        assert_eq!(*sink2.borrow(), None, "cancel never fills the sink");
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::EndModal(c) if *c == Command::CANCEL)),
            "the Dialog ends the modal with CANCEL"
        );
    }
}
