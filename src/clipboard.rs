//! Thread-local clipboard port.
//!
//! Widgets have no access to an `AppCore`, so they reach the clipboard through
//! the free functions [`put`] and [`text`] instead of an injected dependency.
//! Production uses the system clipboard with an in-memory cache for headless
//! environments; tests use a per-thread in-memory port (see [`test_handle`]).

use std::cell::RefCell;
#[cfg(test)]
use std::rc::Rc;
use tv::{Command, Context, MenuItem};

/// The minimal clipboard contract widgets depend on.
pub trait ClipboardPort {
    /// Read the clipboard, or `None` when there is nothing to paste.
    fn get(&self) -> Option<String>;
    /// Write `text`; an empty string clears the clipboard.
    fn put(&self, text: &str);
}

/// Production port: the OS clipboard when a display is available, with a
/// per-thread cache as the headless fallback (CI, SSH, tmux without OSC 52).
pub struct SystemClipboard {
    cache: RefCell<Option<String>>,
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self {
            cache: RefCell::new(None),
        }
    }
}

impl ClipboardPort for SystemClipboard {
    fn get(&self) -> Option<String> {
        // The OS owns the value when it is reachable, so a copy made by
        // another application is seen here. Cache is only the fallback.
        arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok())
            .filter(|s| !s.is_empty())
            .or_else(|| self.cache.borrow().clone().filter(|s| !s.is_empty()))
    }

    fn put(&self, text: &str) {
        *self.cache.borrow_mut() = if text.is_empty() {
            None
        } else {
            Some(text.to_owned())
        };
    }
}

#[cfg(test)]
thread_local! {
    /// Backing store shared by the test port and [`test_handle`].
    static TEST_TEXT: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
}

#[cfg(test)]
#[derive(Clone)]
struct MemoryClipboard {
    text: Rc<RefCell<Option<String>>>,
}

#[cfg(test)]
impl Default for MemoryClipboard {
    fn default() -> Self {
        Self {
            text: TEST_TEXT.with(Rc::clone),
        }
    }
}

#[cfg(test)]
impl ClipboardPort for MemoryClipboard {
    fn get(&self) -> Option<String> {
        self.text.borrow().clone()
    }

    fn put(&self, text: &str) {
        *self.text.borrow_mut() = if text.is_empty() {
            None
        } else {
            Some(text.to_owned())
        };
    }
}

thread_local! {
    static PORT: RefCell<Box<dyn ClipboardPort>> = RefCell::new(default_port());
}

fn default_port() -> Box<dyn ClipboardPort> {
    #[cfg(test)]
    let port: Box<dyn ClipboardPort> = Box::new(MemoryClipboard::default());
    #[cfg(not(test))]
    let port: Box<dyn ClipboardPort> = Box::new(SystemClipboard::default());
    port
}

/// Read text from this thread's clipboard port.
pub fn text() -> Option<String> {
    PORT.with(|p| p.borrow().as_ref().get())
}

/// Write text to this thread's clipboard port.
pub fn put(text: &str) {
    PORT.with(|p| p.borrow().as_ref().put(text))
}

/// The shared tail of every window's context menu: a separator, then the
/// three Edit actions, grayed from the pump's live command snapshot. The
/// chord hints are display-only — the responsible view owns Ctrl+X/C/V.
pub fn context_menu_items(ctx: &Context) -> Vec<MenuItem> {
    let mut items = vec![MenuItem::Separator];
    for (name, command, hint) in [
        ("Cu~t~", Command::CUT, "Ctrl+X"),
        ("~C~opy", Command::COPY, "Ctrl+C"),
        ("~P~aste", Command::PASTE, "Ctrl+V"),
    ] {
        items.push(MenuItem::Command {
            name: name.into(),
            command,
            key_code: None,
            param: Some(hint.into()),
            help_ctx: tv::HelpCtx::NO_CONTEXT,
            disabled: !ctx.command_enabled(command),
        });
    }
    items
}

/// Assertion seam for tests: the cell backing the in-memory port.
#[cfg(test)]
pub fn test_handle() -> Rc<RefCell<Option<String>>> {
    TEST_TEXT.with(Rc::clone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tv::{Command, CommandSet, Context, Deferred, MenuItem, TimerQueue};

    #[test]
    fn put_then_text_round_trips_on_this_thread() {
        put("alpha");
        assert_eq!(text().as_deref(), Some("alpha"));
    }

    #[test]
    fn putting_an_empty_string_clears_the_clipboard() {
        put("alpha");
        put("");
        assert_eq!(text(), None, "empty text means 'nothing to paste'");
    }

    #[test]
    fn test_handle_observes_what_put_wrote() {
        put("beta");
        assert_eq!(test_handle().borrow().as_deref(), Some("beta"));
    }

    #[test]
    fn context_menu_items_follow_the_live_command_set() {
        let mut out = VecDeque::new();
        let mut timers = TimerQueue::new();
        let mut deferred: Vec<Deferred> = Vec::new();
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
        let items = context_menu_items(&ctx);
        assert!(matches!(items.first(), Some(MenuItem::Separator)));
        let commands: Vec<_> = items
            .iter()
            .skip(1)
            .map(|it| match it {
                MenuItem::Command {
                    command, disabled, ..
                } => (*command, *disabled),
                other => panic!("clipboard items are commands: {other:?}"),
            })
            .collect();
        assert_eq!(
            commands,
            [
                (Command::CUT, false),
                (Command::COPY, false),
                (Command::PASTE, false)
            ]
        );

        let mut disabled = CommandSet::new();
        disabled.insert(Command::COPY);
        ctx.set_disabled_commands(disabled);
        let items = context_menu_items(&ctx);
        assert!(
            matches!(
                &items[2],
                MenuItem::Command {
                    command: Command::COPY,
                    disabled: true,
                    ..
                }
            ),
            "a gray command stays in the menu but is disabled"
        );
    }

    #[test]
    fn system_clipboard_falls_back_to_its_cache_without_a_display() {
        // On a headless runner there is no OS clipboard, so `get` must fall
        // back to the cache written by `put`. On a desktop with a live
        // clipboard the OS owns the value and this assertion is not meaningful.
        let os_has_text = arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok())
            .is_some_and(|s| !s.is_empty());
        if os_has_text {
            return;
        }
        let port = SystemClipboard::default();
        port.put("cached");
        assert_eq!(port.get().as_deref(), Some("cached"));
    }
}
