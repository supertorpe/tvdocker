//! VT100/ANSI helpers and views for the embedded shell window .
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, KeyModifiers};
use tv::{Command, Context, DrawCtx, Options, Rect, Role, View, ViewState};

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
pub fn screen_lines(parser: &vt100::Parser) -> Vec<String> {
    parser
        .screen()
        .contents()
        .lines()
        .map(str::to_string)
        .collect()
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
    use crate::testutil::{render_view, text};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tv::Rect;

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
pub struct PtyView {
    st: ViewState,
    state: Rc<RefCell<PtyState>>,
    input_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    resize_tx: tokio::sync::watch::Sender<(u16, u16)>,
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
        }
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
        for (i, line) in screen_lines(&state.parser)
            .iter()
            .enumerate()
            .take(h as usize)
        {
            ctx.put_str(0, i as i32, line, style);
        }
        // Hardware cursor : clamped to THIS view — the parser grid
        // can be stale relative to a just-shrunk window (round-2 bug 2).
        let (row, col, hidden) = cursor_info(state.parser.screen());
        let clamped_col = col.min(dims.0.saturating_sub(1));
        let clamped_row = row.min(dims.1.saturating_sub(1));
        self.st
            .set_cursor(i32::from(clamped_col), i32::from(clamped_row));
        self.st.state.cursor_vis = !hidden;
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        let exited = self.state.borrow().phase.is_finished();
        match ev {
            Event::KeyDown(ke) => {
                let (key, mods) = (ke.key, ke.modifiers);
                if exited {
                    // closes on next keypress after exit.
                    ctx.post(Command::CLOSE);
                    ev.clear();
                    return;
                }
                // NOTE: no F10 guard — the status-line pre-route already
                // turned F10/F1/F3/F5/F6 into commands before dispatch
                // they never arrive as KeyDown.
                if let Some(bytes) = key_to_ansi(key, mods) {
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
                let _ = self.input_tx.send(bytes);
                ev.clear();
            }
            // Event::Command and friends fall through to the window
            // (F3 close, F5 zoom, … keep working while the shell runs).
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
