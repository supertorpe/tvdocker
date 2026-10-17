//! VT100/ANSI helpers and views for the embedded shell window .
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, KeyModifiers, MouseWheel};
use tv::{
    Command, Context, DrawCtx, Options, Point, Rect, Role, Scroller, StateFlag, View, ViewId,
    ViewState,
};

/// F1–F12 → byte sequences (application/CSI as terminals expect).
fn fkey_sequence(n: u8) -> Option<&'static [u8]> {
    Some(match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => return None,
    })
}

/// Translate a tvision-rs key + modifiers into the bytes a shell expects
/// (routing into this function is governed by the shell key
/// policy — status-line keys never arrive here).
pub fn key_to_ansi(key: Key, mods: KeyModifiers) -> Option<Vec<u8>> {
    let ctrl = mods.ctrl;
    let alt = mods.alt;
    let bytes: Vec<u8> = match key {
        Key::Enter => b"\r".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Esc => b"\x1b".to_vec(),
        Key::Up => b"\x1b[A".to_vec(),
        Key::Down => b"\x1b[B".to_vec(),
        Key::Right => b"\x1b[C".to_vec(),
        Key::Left => b"\x1b[D".to_vec(),
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Insert => b"\x1b[2~".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::F(n) => fkey_sequence(n)?.to_vec(),
        Key::Char(c) if ctrl => {
            let lo = c.to_ascii_lowercase();
            let mapped = lo.is_ascii_alphabetic()
                || matches!(lo, '@' | '[' | '\\' | ']' | '^' | '_' | '/' | ' ');
            if !mapped {
                return None;
            }
            vec![lo as u8 & 0x1f]
        }
        Key::Char(c) => c.to_string().into_bytes(),
    };
    if alt && !ctrl {
        let mut out = vec![0x1b];
        out.extend(bytes);
        Some(out)
    } else {
        Some(bytes)
    }
}

/// Current screen contents as rows (draw path).
///
/// One entry PER GRID ROW — not `contents().lines()`, which merges a wrapped
/// row into its continuation and would shift every line below it. The draw path
/// relies on the 1:1 row↔line mapping to place the scrollback viewport.
pub fn screen_lines(parser: &vt100::Parser) -> Vec<String> {
    let (rows, cols) = parser.screen().size();
    parser.screen().rows(0, cols).take(rows as usize).collect()
}

/// Scrollback lines kept per shell session ("scrollback capped").
pub const SCROLLBACK_LINES: usize = 10_000;

/// `(row, col, hidden)` — the vt100 cursor to render (cell grid
/// "drawn with cursor"). `hidden` mirrors the application's DECSCUSR mode;
/// tvision-rs only displays the hardware cursor while the view is focused,
/// so callers set `state.cursor_vis = !hidden` and `set_cursor(col, row)`.
pub fn cursor_info(screen: &vt100::Screen) -> (u16, u16, bool) {
    let (row, col) = screen.cursor_position();
    (row, col, screen.hide_cursor())
}

/// Shell lifecycle (exit → `[process exited with code N]`,
/// closes on next keypress).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyPhase {
    Running,
    Exited { code: Option<i64> },
    Failed { error: String },
}

impl PtyPhase {
    /// Status-strip text; empty while running (transient notes live in the
    /// probe's own field, deviation #5: status text, never a modal).
    pub fn status_text(&self) -> String {
        match self {
            Self::Running => String::new(),
            Self::Exited { code: Some(c) } => format!("[process exited with code {c}]"),
            Self::Exited { code: None } => "[process ended]".to_string(),
            Self::Failed { error } => format!("[error: {error}]"),
        }
    }

    /// Whether the session is over (the view closes on next keypress).
    pub fn is_finished(&self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv::event::{Key, KeyModifiers};

    fn ctrl() -> KeyModifiers {
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        }
    }
    fn alt() -> KeyModifiers {
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        }
    }
    fn plain() -> KeyModifiers {
        KeyModifiers::default()
    }

    #[test]
    fn enter_backspace_tab_escape() {
        assert_eq!(
            key_to_ansi(Key::Enter, plain()).as_deref(),
            Some(b"\r".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Backspace, plain()).as_deref(),
            Some(b"\x7f".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Tab, plain()).as_deref(),
            Some(b"\t".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Esc, plain()).as_deref(),
            Some(b"\x1b".as_slice())
        );
    }

    #[test]
    fn arrows_send_csi_sequences() {
        assert_eq!(
            key_to_ansi(Key::Up, plain()).as_deref(),
            Some(b"\x1b[A".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Down, plain()).as_deref(),
            Some(b"\x1b[B".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Right, plain()).as_deref(),
            Some(b"\x1b[C".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Left, plain()).as_deref(),
            Some(b"\x1b[D".as_slice())
        );
    }

    #[test]
    fn home_end_page_insert_delete() {
        assert_eq!(
            key_to_ansi(Key::Home, plain()).as_deref(),
            Some(b"\x1b[H".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::End, plain()).as_deref(),
            Some(b"\x1b[F".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::PageUp, plain()).as_deref(),
            Some(b"\x1b[5~".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::PageDown, plain()).as_deref(),
            Some(b"\x1b[6~".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Insert, plain()).as_deref(),
            Some(b"\x1b[2~".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::Delete, plain()).as_deref(),
            Some(b"\x1b[3~".as_slice())
        );
    }

    #[test]
    fn f1_through_f4_use_application_sequences() {
        assert_eq!(
            key_to_ansi(Key::F(1), plain()).as_deref(),
            Some(b"\x1bOP".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::F(2), plain()).as_deref(),
            Some(b"\x1bOQ".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::F(3), plain()).as_deref(),
            Some(b"\x1bOR".as_slice())
        );
        assert_eq!(
            key_to_ansi(Key::F(4), plain()).as_deref(),
            Some(b"\x1bOS".as_slice())
        );
    }

    #[test]
    fn f5_through_f12_use_csi_sequences() {
        let expected = [
            (5u8, b"\x1b[15~".as_slice()),
            (6, b"\x1b[17~"),
            (7, b"\x1b[18~"),
            (8, b"\x1b[19~"),
            (9, b"\x1b[20~"),
            (10, b"\x1b[21~"),
            (11, b"\x1b[23~"),
            (12, b"\x1b[24~"),
        ];
        for (n, seq) in expected {
            assert_eq!(
                key_to_ansi(Key::F(n), plain()).as_deref(),
                Some(seq),
                "F{n}"
            );
        }
    }

    #[test]
    fn function_keys_above_f12_return_none() {
        for n in [13u8, 14, 20] {
            assert!(key_to_ansi(Key::F(n), plain()).is_none(), "F{n}");
        }
    }

    #[test]
    fn ctrl_letters_map_to_control_bytes() {
        assert_eq!(key_to_ansi(Key::Char('c'), ctrl()), Some(vec![0x03]));
        assert_eq!(key_to_ansi(Key::Char('D'), ctrl()), Some(vec![0x04]));
        assert_eq!(key_to_ansi(Key::Char('@'), ctrl()), Some(vec![0x00]));
        assert_eq!(key_to_ansi(Key::Char(' '), ctrl()), Some(vec![0x00]));
    }

    #[test]
    fn ctrl_symbols_map_to_control_bytes() {
        assert_eq!(key_to_ansi(Key::Char('['), ctrl()), Some(vec![0x1b]));
        assert_eq!(key_to_ansi(Key::Char('\\'), ctrl()), Some(vec![0x1c]));
        assert_eq!(key_to_ansi(Key::Char(']'), ctrl()), Some(vec![0x1d]));
        assert_eq!(key_to_ansi(Key::Char('/'), ctrl()), Some(vec![0x0f]));
    }

    #[test]
    fn ctrl_unmapped_chars_return_none() {
        assert!(key_to_ansi(Key::Char('é'), ctrl()).is_none());
    }

    #[test]
    fn plain_chars_encode_utf8() {
        assert_eq!(key_to_ansi(Key::Char('x'), plain()), Some(vec![b'x']));
        assert_eq!(key_to_ansi(Key::Char('é'), plain()), Some(vec![0xc3, 0xa9]));
    }

    #[test]
    fn alt_prefixes_escape_unless_ctrl_also_held() {
        assert_eq!(key_to_ansi(Key::Char('x'), alt()), Some(vec![0x1b, b'x']));
        // Ctrl wins over Alt (no double encoding).
        let both = KeyModifiers {
            ctrl: true,
            alt: true,
            ..KeyModifiers::default()
        };
        assert_eq!(key_to_ansi(Key::Char('c'), both), Some(vec![0x03]));
    }

    #[test]
    fn screen_lines_split_the_terminal_contents() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process(b"hello\r\nworld");
        let lines = screen_lines(&parser);
        assert_eq!(lines[0], "hello");
        assert_eq!(lines[1], "world");
    }

    #[test]
    fn screen_lines_respect_set_size() {
        let mut parser = vt100::Parser::new(24, 80, SCROLLBACK_LINES);
        parser.process(b"one\r\ntwo\r\nthree");
        // vt100 0.16: resize lives on the Screen.
        parser.screen_mut().set_size(2, 20);
        let lines = screen_lines(&parser);
        assert!(
            lines.len() <= 2,
            "never more rows than the screen: {}",
            lines.len()
        );
    }

    #[test]
    fn scrollback_cap_is_10k_and_session_survives_overflow() {
        assert_eq!(SCROLLBACK_LINES, 10_000, "scrollback capped at 10k");
        let mut parser = vt100::Parser::new(5, 20, SCROLLBACK_LINES);
        for i in 0..12_000 {
            parser.process(format!("line {i}\r\n").as_bytes());
        }
        let lines = screen_lines(&parser);
        assert!(
            lines.iter().any(|l| l.starts_with("line 119")),
            "newest content retained after overflowing the cap: {lines:?}"
        );
        assert!(lines.len() <= 5, "visible grid bounded by rows");
    }

    #[test]
    fn cursor_info_tracks_position_and_visibility() {
        let mut parser = vt100::Parser::new(5, 20, SCROLLBACK_LINES);
        assert_eq!(cursor_info(parser.screen()), (0, 0, false), "fresh cursor");
        parser.process(b"abc");
        assert_eq!(cursor_info(parser.screen()), (0, 3, false), "after text");
        parser.process(b"\x1b[2;3H");
        assert_eq!(cursor_info(parser.screen()), (1, 2, false), "explicit move");
        parser.process(b"\x1b[?25l");
        assert_eq!(cursor_info(parser.screen()), (1, 2, true), "hidden by app");
        parser.process(b"\x1b[?25h");
        assert_eq!(cursor_info(parser.screen()), (1, 2, false), "shown again");
    }

    #[test]
    fn pty_phase_status_text_matches_spec_format() {
        assert_eq!(PtyPhase::Running.status_text(), "");
        assert_eq!(
            PtyPhase::Exited { code: Some(0) }.status_text(),
            "[process exited with code 0]"
        );
        assert_eq!(
            PtyPhase::Exited { code: Some(3) }.status_text(),
            "[process exited with code 3]"
        );
        assert_eq!(
            PtyPhase::Exited { code: None }.status_text(),
            "[process ended]"
        );
        assert_eq!(
            PtyPhase::Failed {
                error: "boom".into()
            }
            .status_text(),
            "[error: boom]"
        );
        assert!(!PtyPhase::Running.is_finished());
        assert!(PtyPhase::Exited { code: Some(1) }.is_finished());
        assert!(PtyPhase::Failed { error: "x".into() }.is_finished());
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;
    use crate::testutil::{render_view, send_event, send_key_mods, text};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tv::Rect;
    use tv::event::MouseEvent;

    fn state_with(rows: u16, cols: u16, cursor_seq: &[u8]) -> Rc<RefCell<PtyState>> {
        let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_LINES);
        parser.process(cursor_seq);
        Rc::new(RefCell::new(PtyState {
            parser,
            note: String::new(),
            phase: PtyPhase::Running,
            size: (cols, rows),
        }))
    }

    fn pty_view(bounds: Rect, state: Rc<RefCell<PtyState>>) -> PtyView {
        let (input_tx, _input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (resize_tx, _resize_rx) = tokio::sync::watch::channel((80u16, 24u16));
        PtyView::new(bounds, state, input_tx, resize_tx)
    }

    /// 20 lines on a 5-row screen: most of them end up in the scrollback.
    fn scrolled_state() -> Rc<RefCell<PtyState>> {
        let state = state_with(5, 20, b"");
        {
            let mut s = state.borrow_mut();
            for i in 1..=20 {
                s.parser.process(format!("line {i}\r\n").as_bytes());
            }
        }
        state
    }

    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        }
    }

    /// The Bridge's "state changed" broadcast — the only Context the view has
    /// outside key delivery.
    fn refresh() -> Event {
        Event::Broadcast {
            command: crate::bridge::REFRESH,
            source: None,
        }
    }

    fn wheel(dir: MouseWheel) -> Event {
        Event::MouseWheel(MouseEvent {
            wheel: dir,
            ..Default::default()
        })
    }

    #[test]
    fn pty_view_clamps_the_cursor_into_the_view() {
        // Parser grid stays 40 rows with the cursor at row 30, while the view
        // is only 10 rows tall (the stale-size situation a window shrink
        // produced before the compact relayout): the hardware cursor must
        // still land INSIDE the view (round-2 bug 2).
        let state = state_with(40, 100, b"\x1b[31;5H"); // 1-based 31;5 → (30,4)
        let mut v = pty_view(Rect::new(0, 0, 30, 10), state);
        let _ = render_view(30, 10, &mut v);
        let c = v.state().cursor;
        assert!(
            c.y < 10 && c.x < 30,
            "cursor must be clamped into the view, got {c:?}"
        );
        assert_eq!(
            (c.x, c.y),
            (4, 9),
            "clamped to the last view row, original column"
        );
    }

    #[test]
    fn pty_view_draws_screen_but_never_the_status_label() {
        // The status label is StatusView's job; PtyView must not paint it —
        // otherwise it buries the shell content/cursor on that row (round-2
        // bug 1).
        let state = state_with(10, 30, b"hello");
        state.borrow_mut().note = "attached".into();
        let mut v = pty_view(Rect::new(0, 0, 30, 10), Rc::clone(&state));
        let out = text(&render_view(30, 10, &mut v));
        assert!(out.contains("hello"), "screen content drawn: {out:?}");
        assert!(
            !out.contains("[attached]"),
            "no status label inside the terminal: {out:?}"
        );
    }

    #[test]
    fn status_view_renders_note_and_lifecycle_text() {
        let state = state_with(10, 30, b"");
        state.borrow_mut().note = "attached".into();
        let mut sv = StatusView::new(Rect::new(0, 0, 40, 1), Rc::clone(&state));
        let out = text(&render_view(40, 1, &mut sv));
        assert!(out.contains("[attached]"), "note shown: {out:?}");

        // Lifecycle text wins over the transient note .
        state.borrow_mut().phase = PtyPhase::Exited { code: Some(3) };
        let out = text(&render_view(40, 1, &mut sv));
        assert!(
            out.contains("[process exited with code 3]"),
            "exit text shown: {out:?}"
        );
        assert!(!out.contains("[attached]"), "phase takes priority");
    }

    #[test]
    fn scrollback_depth_counts_what_scrolled_off_and_keeps_the_live_offset() {
        let state = scrolled_state();
        let mut s = state.borrow_mut();
        let depth = s.scrollback_depth();
        assert!(
            depth >= 16,
            "20 lines on a 5-row screen leave most of them off-screen: {depth}"
        );
        assert_eq!(
            s.parser.screen().scrollback(),
            0,
            "the probe must restore the live offset"
        );
    }

    #[test]
    fn pty_view_scrolls_into_history_and_returns_to_the_live_screen() {
        let state = scrolled_state();
        let mut v = pty_view(Rect::new(0, 0, 20, 5), Rc::clone(&state));
        let live = text(&render_view(20, 5, &mut v));
        assert!(
            live.contains("line 20"),
            "live screen shows the newest line: {live:?}"
        );

        // The bar range is only reachable from an event — draw has no Context.
        send_event(&mut v, refresh());
        let depth = state.borrow_mut().scrollback_depth() as i32;
        assert_eq!(
            v.scroller.limit().y,
            depth + 5,
            "range = scrollback + rows on screen"
        );

        send_key_mods(&mut v, Key::Home, shift());
        let top = text(&render_view(20, 5, &mut v));
        assert!(top.contains("line 1"), "top of the scrollback: {top:?}");
        assert!(
            !top.contains("line 20"),
            "the live screen scrolled away: {top:?}"
        );
        assert!(
            !v.state().state.cursor_vis,
            "no live cursor while parked in history"
        );

        send_key_mods(&mut v, Key::End, shift());
        let back = text(&render_view(20, 5, &mut v));
        assert!(
            back.contains("line 20"),
            "back on the live screen: {back:?}"
        );
        assert!(
            v.state().state.cursor_vis,
            "the cursor returns with the live screen"
        );
    }

    #[test]
    fn wheel_scrolls_the_scrollback_without_touching_the_shell() {
        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (resize_tx, _resize_rx) = tokio::sync::watch::channel((80u16, 24u16));
        let mut v = PtyView::new(
            Rect::new(0, 0, 20, 5),
            scrolled_state(),
            input_tx,
            resize_tx,
        );
        send_event(&mut v, refresh());
        let before = v.scroller.delta.y;
        assert!(before > 3, "there is history to scroll into: {before}");

        send_event(&mut v, wheel(MouseWheel::Up));
        assert_eq!(v.scroller.delta.y, before - 3, "wheel up = 3 lines");
        assert!(!v.follow, "wheeling up un-follows the live screen");
        assert!(
            input_rx.try_recv().is_err(),
            "the wheel is ours, not shell input"
        );
    }

    #[test]
    fn bare_navigation_keys_stay_shell_input_while_shift_scrolls() {
        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (resize_tx, _resize_rx) = tokio::sync::watch::channel((80u16, 24u16));
        let mut v = PtyView::new(
            Rect::new(0, 0, 20, 5),
            scrolled_state(),
            input_tx,
            resize_tx,
        );

        // Bare ↑ / PageUp belong to the shell (command history, less, …).
        send_key_mods(&mut v, Key::Up, KeyModifiers::default());
        assert_eq!(
            input_rx.try_recv().expect("bare ↑ is shell input"),
            b"\x1b[A".to_vec()
        );
        send_key_mods(&mut v, Key::PageUp, KeyModifiers::default());
        assert_eq!(
            input_rx.try_recv().expect("bare PageUp is shell input"),
            b"\x1b[5~".to_vec()
        );

        // Shift+↑ scrolls the history instead and never reaches the shell.
        send_event(&mut v, refresh());
        let before = v.scroller.delta.y;
        send_key_mods(&mut v, Key::Up, shift());
        assert!(
            v.scroller.delta.y < before,
            "shift+↑ scrolls up: {} < {before}",
            v.scroller.delta.y
        );
        assert!(
            input_rx.try_recv().is_err(),
            "shift+↑ is ours, not shell input"
        );
    }

    #[test]
    fn typing_while_scrolled_up_returns_to_the_live_screen() {
        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (resize_tx, _resize_rx) = tokio::sync::watch::channel((80u16, 24u16));
        let mut v = PtyView::new(
            Rect::new(0, 0, 20, 5),
            scrolled_state(),
            input_tx,
            resize_tx,
        );
        send_event(&mut v, refresh());

        // Park in history.
        send_key_mods(&mut v, Key::Home, shift());
        assert!(!v.follow, "parked in history");
        let out = text(&render_view(20, 5, &mut v));
        assert!(!out.contains("line 20"), "history on screen: {out:?}");

        // A keystroke pulls the viewport back to the live screen …
        send_key_mods(&mut v, Key::Char('l'), KeyModifiers::default());
        assert!(v.follow, "typing re-arms following");
        assert_eq!(
            input_rx
                .try_recv()
                .expect("the key still reaches the shell"),
            b"l".to_vec()
        );
        let out = text(&render_view(20, 5, &mut v));
        assert!(
            out.contains("line 20"),
            "live screen visible again: {out:?}"
        );

        // … and so does a paste.
        send_key_mods(&mut v, Key::Home, shift());
        assert!(!v.follow, "parked again");
        send_event(&mut v, Event::Paste("ls\n".into()));
        assert!(v.follow, "paste re-arms following");
        assert_eq!(
            input_rx.try_recv().expect("the paste reaches the shell"),
            b"ls\n".to_vec()
        );
        let out = text(&render_view(20, 5, &mut v));
        assert!(
            out.contains("line 20"),
            "live screen after the paste: {out:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Shell views (promoted from the probe so they are unit-testable).
// ---------------------------------------------------------------------------

/// Bridge messages: terminal bytes, a transient note, or a lifecycle
/// transition (exit → `[process exited with code N]`).
#[derive(Debug)]
pub enum PtyMsg {
    Bytes(Vec<u8>),
    Note(String),
    Phase(PtyPhase),
}

/// Shared state of one shell session: the parser owns the screen; `note` is
/// transient ("attached"); `phase` is the lifecycle ; `size` is the
/// last `(cols, rows)` pushed to the parser.
pub struct PtyState {
    pub parser: vt100::Parser,
    pub note: String,
    pub phase: PtyPhase,
    pub size: (u16, u16),
}

impl PtyState {
    /// Apply one bridge message.
    pub fn apply(&mut self, msg: PtyMsg) {
        match msg {
            PtyMsg::Bytes(b) => self.parser.process(&b),
            PtyMsg::Note(t) => self.note = t,
            PtyMsg::Phase(p) => self.phase = p,
        }
    }

    /// Lines that have scrolled off the top of the live screen .
    ///
    /// vt100 0.16 keeps the scrollback length private: the only door to it is
    /// `set_scrollback`'s documented clamp ("the value … is clamped to the
    /// actual size of the scrollback"). Probe with an over-large request, read
    /// the clamped offset back, then restore the live position.
    pub fn scrollback_depth(&mut self) -> usize {
        let screen = self.parser.screen_mut();
        let live = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let depth = screen.scrollback();
        screen.set_scrollback(live);
        depth
    }
}

/// One-row lifecycle/status line for a shell window . A SEPARATE
/// child from the terminal, so terminal content — and the hardware cursor —
/// can never be overwritten by the label (round-2 bug 1).
pub struct StatusView {
    st: ViewState,
    state: Rc<RefCell<PtyState>>,
}

impl StatusView {
    pub fn new(bounds: Rect, state: Rc<RefCell<PtyState>>) -> Self {
        Self {
            st: ViewState::new(bounds),
            state,
        }
    }
}

impl View for StatusView {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        ctx.fill(self.st.get_extent(), ' ', ctx.style(Role::ScrollerNormal));
        let state = self.state.borrow();
        let phase_text = state.phase.status_text();
        let (text, role) = if !phase_text.is_empty() {
            let role = if matches!(&state.phase, PtyPhase::Failed { .. }) {
                Role::Error
            } else {
                Role::Warning
            };
            (phase_text, role)
        } else if !state.note.is_empty() {
            (format!("[{}]", state.note), Role::Info)
        } else {
            return;
        };
        ctx.put_str(0, 0, &format!(" {text} "), ctx.style(role));
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// The shell body: draws the vt100 grid + cursor, forwards keys to the exec
/// stdin, and closes the window on the first key after exit .
///
/// The scrollback is exposed through a right-edge [`Scroller`]: Shift+↑↓ /
/// PgUp / PgDn / Home / End, the mouse wheel and the bar drive it — the BARE
/// navigation keys stay ANSI input (the shell needs ↑↓, PageUp, Home…).
/// Typing or pasting while parked in history snaps back to the live screen.
pub struct PtyView {
    st: ViewState,
    state: Rc<RefCell<PtyState>>,
    input_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    resize_tx: tokio::sync::watch::Sender<(u16, u16)>,
    scroller: Scroller,
    /// Viewport pinned to the live bottom. Cleared by any scroll-up; re-armed
    /// by Shift+End, by reaching the end, or by the bar landing there.
    follow: bool,
}

impl PtyView {
    /// Focusable terminal body (Options::default() is selectable:false).
    pub fn new(
        bounds: Rect,
        state: Rc<RefCell<PtyState>>,
        input_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        resize_tx: tokio::sync::watch::Sender<(u16, u16)>,
    ) -> Self {
        let mut st = ViewState::new(bounds);
        st.options = Options {
            selectable: true,
            first_click: true,
            ..Options::default()
        };
        Self {
            st,
            state,
            input_tx,
            resize_tx,
            scroller: Scroller::new(bounds, None, None),
            follow: true,
        }
    }

    /// Link the window's right-edge scroll bar. The id only exists once the
    /// layout inserted the bar, so the shell builder calls this right after
    /// `new` — before insertion; the range is published from the next event
    /// (or from `on_bounds_changed`), where a `Context` is available.
    pub fn set_v_scroll_bar(&mut self, v_bar: Option<ViewId>) {
        self.scroller = Scroller::new(self.st.get_bounds(), None, v_bar);
    }

    /// Rows on screen in this view.
    fn visible(&self) -> i32 {
        self.st.size.y.max(0)
    }

    /// The scrollable buffer: everything scrolled off plus the rows on screen.
    fn content_lines(&self) -> i32 {
        self.state.borrow_mut().scrollback_depth() as i32 + self.visible()
    }

    /// Republish the range to the bar and the position to the viewport. Needs
    /// a `Context`, so it runs from events (REFRESH / keys / resize) — never
    /// from `draw`.
    fn sync(&mut self, ctx: &mut Context) {
        let total = self.content_lines();
        self.scroller.set_limit(0, total, ctx);
        let want = if self.follow {
            (total - self.visible()).max(0)
        } else {
            self.scroller.delta.y
        };
        self.set_offset(want, ctx);
    }

    /// Move the viewport to an absolute first-line index (clamped) and push
    /// the new position to the bar. Landing on the last page re-arms `follow`.
    fn set_offset(&mut self, want: i32, ctx: &mut Context) {
        let max = (self.scroller.limit().y - self.visible()).max(0);
        let want = want.clamp(0, max);
        self.follow = want >= max;
        self.scroller.apply_delta(Point::new(0, want));
        self.scroller.scroll_to(0, want, ctx);
    }

    /// Scroll by `delta` lines (negative = up).
    fn scroll_by(&mut self, delta: i32, ctx: &mut Context) {
        self.set_offset(self.scroller.delta.y.saturating_add(delta), ctx);
    }

    /// Typing while scrolled up snaps back to the live screen — the input and
    /// its echo must stay visible, like in any other terminal. A no-op when
    /// already following (so a plain keystroke costs nothing).
    fn resume(&mut self, ctx: &mut Context) {
        if !self.follow {
            self.set_offset(i32::MAX, ctx);
        }
    }

    /// Shift + navigation → viewport movement. Returns whether the key was a
    /// scroll key, so the caller consumes it instead of typing it into the
    /// shell.
    fn scroll_key(&mut self, key: Key, ctx: &mut Context) -> bool {
        match key {
            Key::Up => self.scroll_by(-1, ctx),
            Key::Down => self.scroll_by(1, ctx),
            Key::PageUp => self.scroll_by(-self.visible().max(1), ctx),
            Key::PageDown => self.scroll_by(self.visible().max(1), ctx),
            Key::Home => self.set_offset(0, ctx),
            Key::End => self.set_offset(i32::MAX, ctx),
            _ => return false,
        }
        true
    }
}

impl View for PtyView {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        let style = ctx.style(Role::ScrollerNormal);
        ctx.fill(self.st.get_extent(), ' ', style);
        let w = self.st.size.x;
        let h = self.st.size.y;
        if w <= 0 || h <= 0 {
            return;
        }
        let mut state = self.state.borrow_mut();
        let dims = (w as u16, h as u16);
        if state.size != dims {
            // Parser reflows immediately; the daemon resize is a watch send
            // (latest-value) debounced in the resize task.
            // vt100 0.16: resizing lives on the Screen, not the Parser.
            state.size = dims;
            state.parser.screen_mut().set_size(dims.1, dims.0);
            let _ = self.resize_tx.send(dims);
        }
        // The scroller addresses the buffer from the TOP (index of the first
        // visible line); vt100 wants how far the window sits above the LIVE
        // bottom: offset = depth - delta. Following is delta at the end, i.e.
        // the live screen.
        let depth = state.scrollback_depth() as i32;
        let delta = if self.follow {
            depth
        } else {
            self.scroller.delta.y.clamp(0, depth)
        };
        state
            .parser
            .screen_mut()
            .set_scrollback((depth - delta) as usize);
        self.scroller.apply_delta(Point::new(0, delta));
        let live = delta == depth;
        // vt100 windows the viewport for us — `contents()` already reflects
        // the offset set above, so draw what it returns, row for row.
        for (i, line) in screen_lines(&state.parser)
            .iter()
            .take(h as usize)
            .enumerate()
        {
            ctx.put_str(0, i as i32, line.as_str(), style);
        }
        // Hardware cursor : clamped to THIS view — the parser grid
        // can be stale relative to a just-shrunk window (round-2 bug 2).
        // Scrolled into history there is no live row to blink on.
        let (row, col, hidden) = cursor_info(state.parser.screen());
        let clamped_col = col.min(dims.0.saturating_sub(1));
        let clamped_row = row.min(dims.1.saturating_sub(1));
        self.st
            .set_cursor(i32::from(clamped_col), i32::from(clamped_row));
        self.st.state.cursor_vis = !hidden && live;
    }

    /// Own flags must track focus — the hardware cursor is only placed for a
    /// focused view with `cursor_vis` — so the embedded Scroller cannot take
    /// this wholesale. It gets every flag EXCEPT `Focused`: forwarding that one
    /// would make an uninserted scroller broadcast the focus pair with no
    /// source. It is the half that shows/hides the bar (Scroller::set_state).
    fn set_state(&mut self, flag: StateFlag, enable: bool, ctx: &mut Context) {
        self.st.set_flag(flag, enable);
        if flag != StateFlag::Focused {
            self.scroller.set_state(flag, enable, ctx);
        }
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // Bar ↔ content: the embedded Scroller has no ViewId, so this
        // inserted composite brokers the sync (ListViewer pattern). Broadcasts
        // reach every sibling — consume NONE of them.
        if let Event::Broadcast { command, source } = *ev {
            let v_bar = self.scroller.v_scroll_bar();
            let own = source.is_some() && source == v_bar;
            if own && command == Command::SCROLL_BAR_CLICKED {
                if let Some(id) = self.st.id() {
                    ctx.request_focus(id);
                }
            } else if own && command == Command::SCROLL_BAR_CHANGED {
                if let Some(id) = self.st.id() {
                    ctx.request_scroll_sync(id, None, v_bar);
                }
            } else if command == crate::bridge::REFRESH {
                // The Bridge just drained PTY bytes into `PtyState`: the
                // scrollback grew, so republish the range — a `Context` lives
                // here, and `draw` has none.
                self.sync(ctx);
            }
            return;
        }
        let exited = self.state.borrow().phase.is_finished();
        match ev {
            Event::KeyDown(ke) => {
                let (key, mods) = (ke.key, ke.modifiers);
                if exited {
                    ctx.post(Command::CLOSE);
                    ev.clear();
                    return;
                }
                // NOTE: no F10 guard — the status-line pre-route already
                // turned F10/F1/F3/F5/F6 into commands before dispatch
                // they never arrive as KeyDown.
                // Shift + navigation scrolls the scrollback; the bare keys
                // stay ANSI input (the shell needs ↑↓, PageUp, Home…).
                if mods.shift && self.scroll_key(key, ctx) {
                    ev.clear();
                    return;
                }
                if let Some(bytes) = key_to_ansi(key, mods) {
                    // Writing while parked in history pulls the viewport back
                    // to the live screen first (resume, above).
                    self.resume(ctx);
                    let _ = self.input_tx.send(bytes);
                    ev.clear();
                }
            }
            Event::Paste(s) => {
                let bytes = s.as_bytes().to_vec();
                if exited {
                    ctx.post(Command::CLOSE);
                    ev.clear();
                    return;
                }
                self.resume(ctx);
                let _ = self.input_tx.send(bytes);
                ev.clear();
            }
            Event::MouseWheel(m) => {
                let delta = match m.wheel {
                    MouseWheel::Up => -3,
                    MouseWheel::Down => 3,
                    _ => 0,
                };
                if delta != 0 {
                    self.scroll_by(delta, ctx);
                    ev.clear();
                }
            }
            // Event::Command and friends fall through to the window
            // (F3 close, F5 zoom, … keep working while the shell runs).
            _ => {}
        }
    }

    /// Bar drag → viewport. The bar's own value is NOT written back (the pump
    /// already owns it; echoing it would re-arm the broker).
    fn apply_scroll_sync(&mut self, _h: Option<i32>, v: Option<i32>, _ctx: &mut Context) {
        let Some(v) = v else {
            return;
        };
        let max = (self.scroller.limit().y - self.visible()).max(0);
        let want = v.clamp(0, max);
        self.follow = want >= max;
        self.scroller.apply_delta(Point::new(0, want));
    }

    fn change_bounds(&mut self, bounds: Rect) {
        self.st.set_bounds(bounds);
        self.scroller.change_bounds(bounds);
    }

    fn on_bounds_changed(&mut self, ctx: &mut Context) {
        self.scroller.on_bounds_changed(ctx);
        self.sync(ctx);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
