//! A one-line view over shared text: the owner mutates the
//! `Rc<RefCell<String>>`, the next draw shows it. Used for explorer
//! breadcrumb/hint rows and the Diagnostics status row.
use std::cell::RefCell;
use std::rc::Rc;
use tv::{DrawCtx, Rect, Role, View, ViewState};

/// A fixed-height, non-selectable line whose text is read at draw time.
pub struct SharedLine {
    st: ViewState,
    text: Rc<RefCell<String>>,
    role: Role,
}

impl SharedLine {
    /// `bounds` is usually a single row; `role` is the theme role the text
    /// is drawn with.
    pub fn new(bounds: Rect, text: Rc<RefCell<String>>, role: Role) -> Self {
        Self {
            st: ViewState::new(bounds),
            text,
            role,
        }
    }
}

impl View for SharedLine {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let text = self.text.borrow();
        ctx.put_str(0, 0, &text, ctx.style(self.role));
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{render_view, text};

    #[test]
    fn draw_reads_the_shared_slot_at_mutator_pace() {
        let slot = Rc::new(RefCell::new("breadcrumb".to_string()));
        let mut line = SharedLine::new(Rect::new(0, 0, 20, 1), Rc::clone(&slot), Role::Disabled);
        let out = text(&render_view(20, 1, &mut line));
        assert!(out.contains("breadcrumb"), "{out:?}");
        // The owner rewrites the slot; the next draw shows the new text.
        *slot.borrow_mut() = "a hint".to_string();
        let out = text(&render_view(20, 1, &mut line));
        assert!(
            out.contains("a hint"),
            "reads the slot at draw time: {out:?}"
        );
    }
}
