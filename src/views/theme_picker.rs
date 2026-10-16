//! Options ▸ Themes… : a modal list of the configured themes with a preview
//! painted in the CANDIDATE palette.
//!
//! Nothing global changes until OK — the preview is drawn from the entry's
//! own [`Theme`], so moving the cursor needs no `Program` access and Esc
//! needs no revert (the picker never called `set_theme`).

use std::cell::Cell;
use std::rc::Rc;

use tv::dialog::{ButtonRowAlign, Dialog};
use tv::event::{Event, Key, MouseWheel};
use tv::widgets::ButtonFlags;
use tv::{Command, Context, DrawCtx, Rect, Role, Theme, View, ViewId, ViewState};

/// One row: the name, its fully resolved palette, and the detail line drawn
/// under the preview.
pub struct ThemeEntry {
    pub name: String,
    pub theme: Theme,
    pub summary: String,
}

impl ThemeEntry {
    pub fn new(name: impl Into<String>, theme: Theme, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            theme,
            summary: summary.into(),
        }
    }
}

/// Fixed dialog geometry (frame included): the preview column needs 43 cells,
/// the name column 24; the OK/Cancel row sits on `height − 3` (`button_row`).
const DIALOG_W: i32 = 70;
const DIALOG_H: i32 = 17;
/// List rows drawn (longer lists scroll; the cursor stays in the window).
const LIST_ROWS: usize = 12;
/// Preview box: `x`..`x + PREVIEW_W`, 7 rows (border + 5 lines).
const PREVIEW_X: i32 = 25;
const PREVIEW_W: i32 = 43;

/// Centered picker rect for a desktop of `desktop` (falls back to the whole
/// area on a screen too small for the dialog).
pub fn picker_rect(desktop: Rect) -> Rect {
    let size = desktop.b - desktop.a;
    let w = DIALOG_W.min(size.x.max(6));
    let h = DIALOG_H.min(size.y.max(6));
    let x = desktop.a.x + (size.x - w) / 2;
    let y = desktop.a.y + (size.y - h) / 2;
    Rect::new(x, y, x + w, y + h)
}

/// Split the detail line under the narrower preview column: the first two
/// headline colours on one line, the rest on the next.
fn split_summary(s: &str) -> (String, String) {
    let mut parts = s.splitn(3, " · ");
    let first = parts.next().unwrap_or("");
    let second = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    let head = if second.is_empty() {
        first.to_string()
    } else {
        format!("{first} · {second}")
    };
    (head, rest.to_string())
}

pub struct ThemePickerDialog {
    dialog: Dialog,
    entries: Rc<Vec<ThemeEntry>>,
    cursor: Rc<Cell<usize>>,
}

impl ThemePickerDialog {
    /// `current` is the name in use; the cursor starts on it.
    pub fn new(bounds: Rect, entries: Vec<ThemeEntry>, current: &str) -> Self {
        let mut dialog = Dialog::new(bounds, Some("Themes".to_string()));
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
        let start = entries.iter().position(|e| e.name == current).unwrap_or(0);
        let entries = Rc::new(entries);
        let cursor = Rc::new(Cell::new(start));
        // LAST: a modal with no explicit focus selects the last inserted
        // selectable child, so the list takes the keys. Its rect stops at the
        // button row so a click on OK/Cancel never hits this view first
        // (children are searched bottom-up).
        let rows = ThemeRowsView {
            st: ViewState::new(Rect::new(1, 1, w - 1, h - 3)),
            entries: Rc::clone(&entries),
            cursor: Rc::clone(&cursor),
            current: current.to_string(),
        };
        dialog.insert_child(Box::new(rows));
        Self {
            dialog,
            entries,
            cursor,
        }
    }

    /// The highlighted entry's name — what OK commits.
    pub fn selected(&self) -> Option<&str> {
        self.entries.get(self.cursor.get()).map(|e| e.name.as_str())
    }

    /// Index the cursor rests on (tests).
    pub fn cursor(&self) -> usize {
        self.cursor.get()
    }
}

/// The name column (left) and the preview column (right); draw-only — the
/// wrapper owns the keys.
struct ThemeRowsView {
    st: ViewState,
    entries: Rc<Vec<ThemeEntry>>,
    cursor: Rc<Cell<usize>>,
    current: String,
}

impl ThemeRowsView {
    /// Paint the candidate theme's window, states, severity and buttons.
    fn draw_preview(&self, ctx: &mut DrawCtx, entry: &ThemeEntry) {
        let t = &entry.theme;
        let frame = t.style(Role::FrameActive); // also the interior fill (tvision)
        let px = PREVIEW_X;
        let py = 0;
        let inner = (PREVIEW_W - 2) as usize;
        let title = " preview ";
        ctx.put_str(
            px,
            py,
            &format!("┌{}{}┐", title, "─".repeat(inner - title.chars().count())),
            frame,
        );
        ctx.fill(
            Rect::new(px + 1, py + 1, px + PREVIEW_W - 1, py + 6),
            ' ',
            frame,
        );
        for row in 1..6 {
            ctx.put_str(px, py + row, "│", frame);
            ctx.put_str(px + PREVIEW_W - 1, py + row, "│", frame);
        }
        let x = px + 2;
        ctx.put_str(
            x,
            py + 1,
            "The quick brown fox",
            t.style(Role::ScrollerNormal),
        );
        ctx.put_str(x, py + 2, "» focused row", t.style(Role::ListFocused));
        ctx.put_str(x, py + 3, "  selected row", t.style(Role::ListSelected));
        let mut sx = x;
        for (label, role) in [
            ("error", Role::Error),
            (" warn", Role::Warning),
            (" info", Role::Info),
            (" ok", Role::Success),
        ] {
            ctx.put_str(sx, py + 4, label, t.style(role));
            sx += label.chars().count() as i32;
        }
        ctx.put_str(x, py + 5, "[ OK ]", t.style(Role::ButtonDefault));
        ctx.put_str(x + 8, py + 5, "[ Cancel ]", t.style(Role::ButtonNormal));
        ctx.put_str(px, py + 6, &format!("└{}┘", "─".repeat(inner)), frame);
        // The headline colours, on the two lines the column fits.
        let (head, tail) = split_summary(&entry.summary);
        let text_style = ctx.style(Role::StaticText);
        ctx.put_str(px, py + 8, &head, text_style);
        if !tail.is_empty() {
            ctx.put_str(px, py + 9, &tail, text_style);
        }
    }
}

impl View for ThemeRowsView {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    /// Click or wheel: the group routes positional events to the child under
    /// the cursor, so the position arrives child-local (the dialog subtracts
    /// this view's origin before delivery). Only the name column selects —
    /// the preview on the right is display-only.
    fn handle_event(&mut self, ev: &mut Event, _ctx: &mut Context) {
        match ev {
            Event::MouseDown(m) if m.buttons.left => {
                let (x, y) = (m.position.x, m.position.y);
                if (1..PREVIEW_X).contains(&x) && (0..LIST_ROWS as i32).contains(&y) {
                    let first = self.cursor.get().saturating_sub(LIST_ROWS - 1);
                    let idx = first + y as usize;
                    if idx < self.entries.len() {
                        self.cursor.set(idx);
                        ev.clear();
                    }
                }
            }
            Event::MouseWheel(m) => {
                let delta = match m.wheel {
                    MouseWheel::Up => -3,
                    MouseWheel::Down => 3,
                    _ => 0,
                };
                if delta != 0 {
                    let last = self.entries.len().saturating_sub(1) as i32;
                    let next = (self.cursor.get() as i32 + delta).clamp(0, last);
                    self.cursor.set(next as usize);
                    ev.clear();
                }
            }
            _ => {}
        }
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        let n = self.entries.len();
        if n == 0 {
            ctx.put_str(1, 1, "No themes defined.", ctx.style(Role::Error));
            return;
        }
        let cur = self.cursor.get().min(n - 1);
        let first = cur.saturating_sub(LIST_ROWS - 1);
        let normal = ctx.style(Role::ListNormal);
        let focused = ctx.style(Role::ListFocused);
        for (row, i) in (first..n.min(first + LIST_ROWS)).enumerate() {
            let entry = &self.entries[i];
            let mark = if i == cur { "»" } else { " " };
            let flag = if entry.name == self.current {
                "✓"
            } else {
                " "
            };
            let style = if i == cur { focused } else { normal };
            ctx.put_str(
                1,
                row as i32,
                &format!("{mark} {flag} {}", entry.name),
                style,
            );
        }
        ctx.put_str(
            1,
            LIST_ROWS as i32,
            "↑↓ · type to jump",
            ctx.style(Role::Info),
        );
        self.draw_preview(ctx, &self.entries[cur]);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl View for ThemePickerDialog {
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
        if let Event::KeyDown(ke) = ev {
            let last = self.entries.len().saturating_sub(1);
            let cur = self.cursor.get().min(last);
            match ke.key {
                Key::Down => self.cursor.set((cur + 1).min(last)),
                Key::Up => self.cursor.set(cur.saturating_sub(1)),
                Key::PageDown => self.cursor.set((cur + 6).min(last)),
                Key::PageUp => self.cursor.set(cur.saturating_sub(6)),
                Key::Home => self.cursor.set(0),
                Key::End => self.cursor.set(last),
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
                Key::Char(c) => {
                    // Type-to-select: jump to the entry whose name starts with
                    // `c`, searching from the cursor and wrapping. An
                    // unmatched letter is NOT ours — let it fall through.
                    let n = self.entries.len();
                    let start = self.cursor.get().min(n.saturating_sub(1));
                    // `find` yields the STEP, not the index — map it back
                    // through the cursor so the search really wraps.
                    if let Some(step) =
                        (0..n).find(|&step| self.entries[(start + step) % n].name.starts_with(c))
                    {
                        self.cursor.set((start + step) % n);
                        ev.clear();
                    }
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
        // The modal contract: the answer may arrive as a command as well.
        if let Event::Command(cmd) = ev {
            match *cmd {
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

    /// The pump reaches INTO this wrapper by id (tracked mouse deliveries,
    /// the scroll-sync broker); the base `find_mut` returns `None`.
    fn find_mut(&mut self, id: ViewId) -> Option<&mut dyn View> {
        self.dialog.find_mut(id)
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

    fn entries() -> Vec<ThemeEntry> {
        theme::PRESETS
            .iter()
            .map(|name| {
                ThemeEntry::new(
                    *name,
                    theme::resolve(&Default::default(), name),
                    theme::summary_for(&Default::default(), name),
                )
            })
            .collect()
    }

    fn dialog(current: &str) -> ThemePickerDialog {
        ThemePickerDialog::new(Rect::new(0, 0, DIALOG_W, DIALOG_H), entries(), current)
    }

    fn drive(dlg: &mut ThemePickerDialog, ev: Event) -> Vec<Deferred> {
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

    fn key(dlg: &mut ThemePickerDialog, key: Key) -> Vec<Deferred> {
        drive(dlg, Event::KeyDown(KeyEvent::new(key, Default::default())))
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
    fn cursor_starts_on_the_selection_and_walks_the_list() {
        let mut dlg = dialog("dark");
        assert_eq!(dlg.selected(), Some("dark"), "opens on the theme in use");
        key(&mut dlg, Key::Down);
        assert_eq!(dlg.selected(), Some("light"));
        key(&mut dlg, Key::End);
        assert_eq!(dlg.selected(), Some("terminal"), "End = last");
        key(&mut dlg, Key::Home);
        assert_eq!(dlg.selected(), Some("classic"), "Home = first");
        key(&mut dlg, Key::Up);
        assert_eq!(dlg.selected(), Some("classic"), "no wrap past the top");
        key(&mut dlg, Key::PageDown);
        assert_eq!(
            dlg.selected(),
            Some("terminal"),
            "page is clamped to the last entry"
        );
        key(&mut dlg, Key::PageUp);
        assert_eq!(dlg.selected(), Some("classic"), "page up clamps at the top");
    }

    #[test]
    fn clicking_a_name_selects_it_and_the_preview_column_does_not() {
        let mut dlg = dialog("dark"); // cursor starts on the selection
        // The rows view origin is (1,1): dialog-local (5, 3) arrives
        // child-local (4, 2) ⇒ visible row 2 ⇒ entries[2] ("light").
        drive(&mut dlg, mouse_down_at(5, 3));
        assert_eq!(dlg.selected(), Some("light"), "the click selects");

        // The preview column (x ≥ PREVIEW_X) is display-only.
        drive(&mut dlg, mouse_down_at(PREVIEW_X + 2, 3));
        assert_eq!(
            dlg.selected(),
            Some("light"),
            "clicking the preview does not move the selection"
        );

        // The wheel scrolls the name list (clamped to the last entry).
        drive(&mut dlg, wheel(MouseWheel::Down));
        assert_eq!(dlg.selected(), Some("terminal"), "wheel down clamps");
        drive(&mut dlg, wheel(MouseWheel::Up));
        assert_eq!(
            dlg.selected(),
            Some("classic"),
            "wheel up = 3, clamped at 0"
        );
    }

    #[test]
    fn type_to_select_jumps_to_the_matching_entry() {
        // cursor starts on the selection ("dark").
        let mut dlg = dialog("dark");
        key(&mut dlg, Key::Char('l'));
        assert_eq!(dlg.selected(), Some("light"), "matches forward");
        key(&mut dlg, Key::Char('c'));
        assert_eq!(dlg.selected(), Some("classic"), "wraps around");
        key(&mut dlg, Key::Char('z'));
        assert_eq!(dlg.selected(), Some("classic"), "unmatched letter ignored");
        key(&mut dlg, Key::Char('d'));
        assert_eq!(
            dlg.selected(),
            Some("dark"),
            "matches itself from the cursor"
        );
    }

    #[test]
    fn enter_commits_and_esc_cancels() {
        let mut dlg = dialog("classic");
        key(&mut dlg, Key::Down);
        let deferred = key(&mut dlg, Key::Enter);
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::EndModal(c) if *c == Command::OK)),
            "Enter → EndModal(OK)"
        );
        assert_eq!(dlg.selected(), Some("dark"), "the moved-to entry commits");

        let mut dlg = dialog("classic");
        let deferred = key(&mut dlg, Key::Esc);
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::EndModal(c) if *c == Command::CANCEL)),
            "Esc → EndModal(CANCEL)"
        );
    }

    #[test]
    fn the_preview_is_painted_in_the_candidates_palette() {
        use crate::testutil::{render_view, text};
        let mut dlg = dialog("dark");
        let handle = render_view(DIALOG_W as u16, DIALOG_H as u16, &mut dlg);
        // The preview's top border sits at local (25, 0) inside the child that
        // starts at (1, 1) ⇒ screen (26, 1).
        let buf = handle.buffer();
        let cell = buf.get(26, 1);
        let dark_bg = theme::resolve(&Default::default(), "dark")
            .style(Role::FrameActive)
            .bg;
        assert_eq!(
            cell.style().bg,
            dark_bg,
            "the box shows the CANDIDATE background, not the active theme"
        );
        let out = text(&handle);
        assert!(out.contains("preview"), "box titled: {out:?}");
        assert!(out.contains("dark"), "the entry is listed: {out:?}");
        assert!(out.contains("↑↓ · type to jump"), "hint: {out:?}");
        assert!(
            out.contains("bg #1e1e2e"),
            "the headline colours sit under the preview: {out:?}"
        );
    }
}
