//! One-line text filter. Communicates through an
//! `Rc<RefCell<String>>` sink — readers compare the sink to detect changes;
//! no broadcast (decision log).
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key};
use tv::{Context, DrawCtx, Options, Rect, Role, View, ViewState};

/// One-line editable filter that mirrors its text into a shared sink.
pub struct FilterBar {
    st: ViewState,
    text: String,
    sink: Rc<RefCell<String>>,
    placeholder: &'static str,
}

impl FilterBar {
    /// Hard cap on stored characters (bounded memory).
    pub const MAX_LEN: usize = 128;

    pub fn new(bounds: Rect, sink: Rc<RefCell<String>>, placeholder: &'static str) -> Self {
        let mut st = ViewState::new(bounds);
        st.options = Options {
            selectable: true,
            first_click: true,
            ..Options::default()
        };
        st.state.cursor_vis = true;
        // The sink may already carry text (CLI prefill,): show it
        // instead of the placeholder, caret after it, capped to MAX_LEN.
        let text: String = sink.borrow().chars().take(Self::MAX_LEN).collect();
        st.set_cursor(1 + text.chars().count() as i32, 0);
        Self {
            st,
            text,
            sink,
            placeholder,
        }
    }

    /// Current text.
    pub fn value(&self) -> &str {
        &self.text
    }

    fn sync(&mut self) {
        *self.sink.borrow_mut() = self.text.clone();
        // View-local caret: one cell of margin + char count.
        self.st.set_cursor(1 + self.text.chars().count() as i32, 0);
    }

    /// Clear the line and mirror it into the sink. Public so window-level
    /// Esc chains (rung 2) can clear the filter, not just this
    /// view's own Ctrl-U/Esc handlers.
    pub fn clear(&mut self) {
        self.text.clear();
        self.sync();
    }

    fn delete_word(&mut self) {
        while self.text.ends_with(' ') {
            self.text.pop();
        }
        let cut = self.text.rfind(' ').map_or(0, |i| i + 1);
        self.text.truncate(cut);
        self.sync();
    }

    /// Delete the previous character. Handles both spellings of the
    /// Backspace key: `Key::Backspace` (terminals sending byte 0x7F) and
    /// Ctrl+H (terminals sending byte 0x08, e.g. QTerminal's fallback —
    /// see keybindings).
    fn backspace(&mut self) {
        self.text.pop();
        self.sync();
    }
}

impl View for FilterBar {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        ctx.fill(self.st.get_extent(), ' ', ctx.style(Role::InputSurface));
        let style = if self.text.is_empty() {
            ctx.style(Role::InputInactive)
        } else if ctx.owner_active() {
            ctx.style(Role::InputNormal)
        } else {
            ctx.style(Role::InputInactive)
        };
        if self.text.is_empty() {
            ctx.put_str(1, 0, self.placeholder, style);
        } else {
            ctx.put_str(1, 0, &self.text, style);
        }
    }

    fn handle_event(&mut self, ev: &mut Event, _ctx: &mut Context) {
        match ev {
            Event::KeyDown(ke) => {
                let (key, ctrl) = (ke.key, ke.modifiers.ctrl);
                if ctrl {
                    match key {
                        Key::Char('u') | Key::Char('U') => {
                            self.clear();
                            ev.clear();
                        }
                        Key::Char('w') | Key::Char('W') => {
                            self.delete_word();
                            ev.clear();
                        }
                        Key::Char('h') | Key::Char('H') => {
                            self.backspace();
                            ev.clear();
                        }
                        _ => {}
                    }
                } else {
                    match key {
                        Key::Char(c) if !c.is_control() => {
                            if self.text.chars().count() < Self::MAX_LEN {
                                self.text.push(c);
                                self.sync();
                            }
                            ev.clear();
                        }
                        Key::Backspace => {
                            self.backspace();
                            ev.clear();
                        }
                        Key::Esc if !self.text.is_empty() => {
                            self.clear();
                            ev.clear();
                        }
                        _ => {} // Enter/Tab/arrows/F-keys bubble
                    }
                }
            }
            Event::MouseDown(m) => {
                if m.buttons.left {
                    ev.clear();
                }
            }
            Event::MouseWheel(_) => {
                ev.clear();
            }
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{render_view, send_key, send_key_mods, text};
    use tv::event::{Key, KeyModifiers};

    fn bar() -> (FilterBar, std::rc::Rc<std::cell::RefCell<String>>) {
        let sink = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let fb = FilterBar::new(
            Rect::new(0, 0, 20, 1),
            std::rc::Rc::clone(&sink),
            "type to filter",
        );
        (fb, sink)
    }

    #[test]
    fn typing_appends_and_updates_the_sink() {
        let (mut fb, sink) = bar();
        for c in ["d", "b"] {
            send_key(&mut fb, Key::Char(c.chars().next().unwrap()));
        }
        assert_eq!(fb.value(), "db");
        assert_eq!(*sink.borrow(), "db");
    }

    #[test]
    fn backspace_removes_the_last_char_and_syncs() {
        let (mut fb, sink) = bar();
        send_key(&mut fb, Key::Char('x'));
        send_key(&mut fb, Key::Backspace);
        assert_eq!(fb.value(), "");
        assert_eq!(*sink.borrow(), "");
    }

    #[test]
    fn ctrl_h_backspaces_like_the_backspace_key() {
        // Terminal compatibility: terminals that send byte 0x08 for the
        // Backspace key (QTerminal's keytab fallback) reach crossterm as
        // Ctrl+H — oh-my-docker accepts both spellings, so the filter must
        // too (keybindings).
        let (mut fb, sink) = bar();
        for c in "abc".chars() {
            send_key(&mut fb, Key::Char(c));
        }
        send_key_mods(
            &mut fb,
            Key::Char('h'),
            KeyModifiers {
                ctrl: true,
                ..Default::default()
            },
        );
        assert_eq!(fb.value(), "ab", "Ctrl+H deletes the previous char");
        assert_eq!(*sink.borrow(), "ab");
    }

    #[test]
    fn ctrl_u_clears_the_whole_line() {
        let (mut fb, sink) = bar();
        for c in "hello".chars() {
            send_key(&mut fb, Key::Char(c));
        }
        send_key_mods(
            &mut fb,
            Key::Char('u'),
            KeyModifiers {
                ctrl: true,
                ..Default::default()
            },
        );
        assert_eq!(fb.value(), "");
        assert_eq!(*sink.borrow(), "");
    }

    #[test]
    fn esc_clears_only_when_nonempty_and_bubbles_when_empty() {
        let (mut fb, _sink) = bar();
        send_key(&mut fb, Key::Char('q'));
        send_key(&mut fb, Key::Esc);
        assert_eq!(fb.value(), "", "Esc clears a non-empty line");
        crate::testutil::with_ctx(|ctx| {
            let mut ev =
                Event::KeyDown(tv::event::KeyEvent::new(Key::Esc, KeyModifiers::default()));
            fb.handle_event(&mut ev, ctx);
            assert!(!ev.is_nothing(), "empty Esc bubbles to the window chain");
        });
    }

    #[test]
    fn enter_and_tab_always_bubble() {
        let (mut fb, _sink) = bar();
        for key in [Key::Enter, Key::Tab] {
            crate::testutil::with_ctx(|ctx| {
                let mut ev = Event::KeyDown(tv::event::KeyEvent::new(key, KeyModifiers::default()));
                fb.handle_event(&mut ev, ctx);
                assert!(!ev.is_nothing(), "{key:?} must bubble");
            });
        }
    }

    #[test]
    fn input_is_capped_at_max_len() {
        let (mut fb, _sink) = bar();
        for _ in 0..FilterBar::MAX_LEN + 10 {
            send_key(&mut fb, Key::Char('a'));
        }
        assert_eq!(fb.value().chars().count(), FilterBar::MAX_LEN);
    }

    #[test]
    fn draw_shows_placeholder_then_text() {
        let (mut fb, _sink) = bar();
        let out = text(&render_view(20, 1, &mut fb));
        assert!(out.contains("type to filter"), "placeholder: {out:?}");
        send_key(&mut fb, Key::Char('d'));
        send_key(&mut fb, Key::Char('b'));
        let out = text(&render_view(20, 1, &mut fb));
        assert!(
            out.contains("db") && !out.contains("type to filter"),
            "text: {out:?}"
        );
    }

    #[test]
    fn prefilled_sink_is_shown_from_the_start() {
        // Gate C: `containers::build` prefills the SINK before constructing
        // the bar (CLI prefill) — the line must show that text,
        // not the placeholder, and the caret must sit after it.
        let sink = std::rc::Rc::new(std::cell::RefCell::new("web".to_string()));
        let mut fb = FilterBar::new(
            Rect::new(0, 0, 20, 1),
            std::rc::Rc::clone(&sink),
            "type to filter",
        );
        assert_eq!(fb.value(), "web", "text comes from the prefilled sink");
        let out = text(&render_view(20, 1, &mut fb));
        assert!(
            out.contains("web") && !out.contains("type to filter"),
            "prefill drawn, placeholder replaced: {out:?}"
        );
        assert_eq!(fb.state().cursor.x, 4, "caret after 'web'");
        // First keystroke appends, never clobbers the prefill.
        send_key(&mut fb, Key::Char('s'));
        assert_eq!(fb.value(), "webs");
        assert_eq!(*sink.borrow(), "webs");
    }

    #[test]
    fn sink_longer_than_max_len_is_capped_on_construction() {
        let long = "a".repeat(FilterBar::MAX_LEN + 10);
        let sink = std::rc::Rc::new(std::cell::RefCell::new(long));
        let fb = FilterBar::new(
            Rect::new(0, 0, 20, 1),
            std::rc::Rc::clone(&sink),
            "type to filter",
        );
        assert_eq!(fb.value().chars().count(), FilterBar::MAX_LEN);
    }

    #[test]
    fn mouse_click_is_consumed() {
        let (mut fb, _sink) = bar();
        crate::testutil::with_ctx(|ctx| {
            let mut ev = Event::MouseDown(tv::event::MouseEvent {
                position: tv::Point::new(3, 0),
                buttons: tv::event::MouseButtons {
                    left: true,
                    ..Default::default()
                },
                ..Default::default()
            });
            fb.handle_event(&mut ev, ctx);
            assert!(ev.is_nothing(), "click on the bar is consumed");
        });
    }
}
