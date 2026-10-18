//! Application chrome  and runtime wiring : the
//! Containers desktop, dynamic status line, menu bar, the `run_app`
//! command loop, plus `build`/`run` — shared core, bounded channels, and
//! the poller spawn.
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::{broadcast, mpsc};
use tv::status::StatusItemsBuilder;
use tv::{
    Command, CrosstermBackend, Key, KeyEvent, KeyModifiers, Menu, MenuBar, MessageBoxButtons,
    MessageBoxKind, Program, Rect, StatusDef, StatusLine, SystemClock, View, alt,
};

use crate::core::{ActionReq, AppCore, AppMsg, TaskReq};
use crate::domain;
use crate::domain::config::Keybindings;

/// Help → Shortcuts (F1): opens the shortcuts box (`SHORTCUTS_TEXT`).
pub const CMD_HELP: Command = Command::custom("tvdocker.help");
/// Help → About.
pub const CMD_ABOUT: Command = Command::custom("tvdocker.about");
/// Help → Check for updates… (parity "Self-update" · §12): an
/// explicit user action, so `run_program` does the short blocking check and
/// reports it in a modal.
pub const CMD_CHECK_UPDATES: Command = Command::custom("tvdocker.check_updates");
/// Docker → Containers: open the window, or focus it if it is already
/// open (menu: "each opens or focuses that resource window").
pub const CMD_OPEN_CONTAINERS: Command = Command::custom("tvdocker.open_containers");
/// Docker → Images: open the window, or focus it if already open
/// (menu: "each opens or focuses that resource window").
pub const CMD_OPEN_IMAGES: Command = Command::custom("tvdocker.open_images");
/// Docker → Networks: open or focus .
pub const CMD_OPEN_NETWORKS: Command = Command::custom("tvdocker.open_networks");
/// Docker → Volumes: open or focus .
pub const CMD_OPEN_VOLUMES: Command = Command::custom("tvdocker.open_volumes");
/// Docker → Events: open or focus .
pub const CMD_OPEN_EVENTS: Command = Command::custom("tvdocker.open_events");
/// Docker → Statistics: open or focus .
pub const CMD_OPEN_STATISTICS: Command = Command::custom("tvdocker.open_statistics");
/// Help-context entry: select every FILTERED row of the focused
/// table ("Ctrl+A — select all filtered rows"). The status
/// item binds the key; the window's Command arm performs it.
pub const CMD_SELECT_ALL: Command = Command::custom("tvdocker.select_all");

/// Two command namespaces — `Command` cannot
/// carry a payload (`command.rs:56`), and the desktop intercepts
/// commands BEFORE its children, so data rides `AppCore::pending_targets`:
/// 1. `CMD_REQ_*` — bound on the status line / context menu; the
///    focused RESOURCE window stores the target ids and re-posts the
///    second hop.
/// 2. `CMD_OPEN_*`/`CMD_*` — intercepted by the desktop: open windows,
///    show dialogs, queue `TaskReq`s.
///
/// Window requests (hop 1 — handled by views/resource.rs).
pub const CMD_REQ_DETAILS: Command = Command::custom("tvdocker.req_details");
pub const CMD_REQ_LOGS: Command = Command::custom("tvdocker.req_logs");
pub const CMD_REQ_SHELL: Command = Command::custom("tvdocker.req_shell");
pub const CMD_REQ_DELETE: Command = Command::custom("tvdocker.req_delete");
pub const CMD_REQ_START: Command = Command::custom("tvdocker.req_start");
pub const CMD_REQ_STOP: Command = Command::custom("tvdocker.req_stop");
/// Images-only actions: the run form, and the two batch
/// sweeps that carry no targets.
pub const CMD_REQ_RUN: Command = Command::custom("tvdocker.req_run");
pub const CMD_REQ_DANGLING: Command = Command::custom("tvdocker.req_dangling");
pub const CMD_REQ_PRUNE: Command = Command::custom("tvdocker.req_prune");
/// Explorer openers: the Containers context menu and the
/// Volumes `Enter`/menu both ride the two-hop layer.
pub const CMD_REQ_EXPLORER: Command = Command::custom("tvdocker.req_explorer");
pub const CMD_REQ_VOLUME_EXPLORER: Command = Command::custom("tvdocker.req_volume_explorer");
pub const CMD_EXPLORER: Command = Command::custom("tvdocker.explorer");
pub const CMD_VOLUME_EXPLORER: Command = Command::custom("tvdocker.volume_explorer");
/// Explorer from the Images window (image-explorer): `CMD_REQ_*` is
/// the context-menu request (hop 1); `CMD_IMAGE_EXPLORER` is the desktop's
/// opener (hop 2).
pub const CMD_REQ_IMAGE_EXPLORER: Command = Command::custom("tvdocker.req_image_explorer");
pub const CMD_IMAGE_EXPLORER: Command = Command::custom("tvdocker.image_explorer");
/// Layers from the Images window (image-layer explorer): `CMD_REQ_*` is
/// the context-menu request (hop 1); `CMD_LAYERS` is the desktop's opener.
pub const CMD_REQ_IMAGE_LAYERS: Command = Command::custom("tvdocker.req_image_layers");
pub const CMD_LAYERS: Command = Command::custom("tvdocker.layers");
/// Diagnostics: `CMD_REQ_*` is the context-menu request that
/// rides the two-hop layer; `CMD_DIAGNOSTICS` is the desktop's opener.
pub const CMD_REQ_DIAGNOSTICS: Command = Command::custom("tvdocker.req_diagnostics");
pub const CMD_DIAGNOSTICS: Command = Command::custom("tvdocker.diagnostics");
/// Desktop intercepts (hop 2 — handled by views/desktop.rs).
pub const CMD_OPEN_DETAILS: Command = Command::custom("tvdocker.open_details");
pub const CMD_OPEN_LOGS: Command = Command::custom("tvdocker.open_logs");
pub const CMD_SHELL_CONFIG: Command = Command::custom("tvdocker.shell_config");
pub const CMD_SHELL_SUBMIT: Command = Command::custom("tvdocker.shell_submit");
pub const CMD_DELETE: Command = Command::custom("tvdocker.delete");
pub const CMD_DELETE_CONFIRMED: Command = Command::custom("tvdocker.delete_confirmed");
pub const CMD_START: Command = Command::custom("tvdocker.start");
pub const CMD_STOP: Command = Command::custom("tvdocker.stop");
/// Desktop intercept: open the run form for the queued image.
pub const CMD_RUN: Command = Command::custom("tvdocker.run");
/// Desktop intercept: the run form's OK answer (the
/// `CMD_SHELL_SUBMIT` pattern).
pub const CMD_RUN_SUBMIT: Command = Command::custom("tvdocker.run_submit");
/// Logs-window local commands (no desktop hop — the window owns them).
pub const CMD_TOGGLE_FOLLOW: Command = Command::custom("tvdocker.toggle_follow");
pub const CMD_TOGGLE_TIMESTAMPS: Command = Command::custom("tvdocker.toggle_timestamps");
pub const CMD_EXPORT_LOGS: Command = Command::custom("tvdocker.export_logs");
/// The Settings window asks every status line to rebuild from the config
/// (settings — live reload on save). Broadcast, not dispatched to
/// one view: there may be a status line per screen.
pub const CMD_RELOAD_STATUS: Command = Command::custom("tvdocker.reload_status");
/// Open the Settings window (settings), or focus it if already open.
pub const CMD_SETTINGS: Command = Command::custom("tvdocker.settings");
/// Open the theme picker (Options ▸ Themes…). The *selection* is minted per
/// name inside the picker — `Command` is a `&'static str`, so the modal hands
/// the chosen name back instead of posting one command per theme.
pub const CMD_THEMES: Command = Command::custom("tvdocker.themes");
/// Open the palette editor (Options ▸ Edit colors…). The edited working copy
/// comes back through `Program::exec_view_with`, then the handler applies it
/// and offers "save as theme" (tvision's own editor has no read-back, so its
/// output could never be persisted).
pub const CMD_THEME_EDITOR: Command = Command::custom("tvdocker.theme_edit");
/// The Settings window's save key (F2 — the "the action for this window"
/// convention; Logs uses F7, Images F2).
pub const CMD_SETTINGS_SAVE: Command = Command::custom("tvdocker.settings_save");
/// The answer route for the "discard unsaved settings?" box.
pub const CMD_SETTINGS_CLOSE_CONFIRM: Command = Command::custom("tvdocker.settings_close_confirm");

/// AppMsg channel capacity: poller → bridge (full ⇒
/// drop new — the next poll supersedes).
pub const APP_CHANNEL_CAP: usize = 1024;
/// ActionReq broadcast capacity (menu → EVERY poller).
pub const ACTION_CHANNEL_CAP: usize = 16;

/// Help → About body: the project, the running version, and the
/// license.
pub fn about_text() -> String {
    format!(
        "tvdocker {} — multi-window Docker TUI\n\
        Built on Turbo Vision (tvision-rs).\n\
        Licensed under MIT.",
        env!("CARGO_PKG_VERSION")
    )
}

/// Help-box (F1) body: global keys plus the table sort/multi-select
/// bindings. `run_program`'s `CMD_HELP` handler renders this text, and the
/// `f1_help_documents_the_table_sort_keys` gate test asserts both that F1
/// opens the box and that this content documents the sort keys.
pub const SHORTCUTS_TEXT: &str = "\
    Tables: ←/→ cycle the sort column, Shift+←/→ flip\n\
    its direction, Space selects a row;\n\
    Ctrl-A selects all; Shift+F10\n\
    opens the row menu.\n\n\
    Containers: F2 Logs · F4 Shell · Ctrl+E Explorer ·\n\
    Enter Details · Del Delete (confirm);\n\
    F7 pauses log follow.\n\n\
    Images: F2 Run · Ctrl+E Explorer · Ctrl+L Layers · Del Remove ·\n\
    menu: dangling/prune.\n\n\
    Networks: Del Delete.\n\
    Volumes: Enter Explorer · Del Delete.\n\
    Explorer: Tab panels · / filter · n/N new · r rename · X copy · Del delete.\n\n\
    Diagnose: right-click a container →\n\
    Diagnose…, one window per container.\n\n\
    Settings: Options → Settings… edits\n\
    tvdocker.toml; F2 saves + live-reloads.\n\n\
    Esc closes menus/dialogs; inside a shell window\n\
    Esc, Tab, arrows and Ctrl-keys go to the terminal.";

/// A `Ctrl`-chord `KeyEvent`.
fn ctrl_chord(k: Key) -> KeyEvent {
    KeyEvent::new(
        k,
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    )
}

/// The chord a `[keybindings]` field overrides, or the default when it is
/// unset (settings). The FIRST configured chord wins; an
/// unparsable list, and any value §7.4 forbids, fall back to the default
/// rather than leaving the action unreachable (or stealing a fixed key).
/// Everything else about the entry — its label, whether it is
/// visible, its position in the bar — is untouched, so a user with no
/// `[keybindings]` gets byte-identical behaviour.
fn k(configured: &[String], default: KeyEvent) -> KeyEvent {
    // §7.4: a value the save handler rejects must not take effect, and an
    // unparsable one must not leave the action unreachable — in both cases
    // the DEFAULT chord stands, modifiers included. `key_events` only carries
    // a `Key`, so its internal fallback would strip the default's modifiers
    // (turning Alt-X into a bare `x` that quits on a keystroke); the caller
    // therefore owns the fallback.
    let usable: Vec<String> = configured
        .iter()
        .filter(|v| crate::domain::config::rebindable(v))
        .cloned()
        .collect();
    if !usable
        .iter()
        .any(|v| crate::domain::config::parse_key(v).is_some())
    {
        return default;
    }
    crate::domain::config::key_events(&usable, &[default.key])
        .0
        .into_iter()
        .next()
        .unwrap_or(default)
}

/// The five always-visible hotspots — "always" list
fn status_always(d: StatusItemsBuilder, kb: &Keybindings) -> StatusItemsBuilder {
    d.item(
        "~F1~ Help",
        k(&kb.help, KeyEvent::from(Key::F(1))),
        CMD_HELP,
    )
    .item("~F3~ Close", KeyEvent::from(Key::F(3)), Command::CLOSE)
    .item("~F5~ Zoom", KeyEvent::from(Key::F(5)), Command::ZOOM)
    .item("~F6~ Next", KeyEvent::from(Key::F(6)), Command::NEXT)
    .item("~F10~ Menu", KeyEvent::from(Key::F(10)), Command::MENU)
}

/// Bound-but-HIDDEN extras: a `key_item` fires the
/// pre-route like any item while consuming no width
/// (`status_line.rs:319` — text-less items draw nothing). Alt-X and
/// the two window chords ride here in the contextual defs; the
/// universal def SHOWS Alt-X separately.
fn status_hidden(d: StatusItemsBuilder, kb: &Keybindings) -> StatusItemsBuilder {
    d.key_item(k(&kb.quit, alt('x')), Command::QUIT)
        .key_item(
            KeyEvent::new(
                Key::F(5),
                KeyModifiers {
                    ctrl: true,
                    ..KeyModifiers::default()
                },
            ),
            Command::RESIZE,
        )
        .key_item(
            KeyEvent::new(
                Key::F(6),
                KeyModifiers {
                    shift: true,
                    ..KeyModifiers::default()
                },
            ),
            Command::PREV,
        )
}

/// Status line : four defs, first match wins
/// (`status_line.rs:214`) — full layout:
///
/// 1. Containers: visible `F1…F10` + `F2 Logs` + `F4 Shell`
///    (the example), hidden: Alt-X, chords, `^A`, Enter,
///    Del, Ctrl+O — all bound.
/// 2. The three picker tables (Images/Networks/Volumes): visible
///    `F1…F10`, hidden Alt-X/chords/Ctrl+O (no `^A` — §5 scopes it
///    to Containers). Events/Statistics: no def (universal).
/// 3. Logs: visible `F1…F10` + `F7 Follow`, hidden Alt-X/chords
///    (no `^A` — logs have no selection, a binding would be dead).
/// 4. Universal: the bar with Alt-X visible.
pub fn init_status_line(r: Rect) -> Option<StatusLine> {
    let kb = crate::domain::config::TvdockerConfig::load().0.keybindings;
    init_status_line_with(r, &kb)
}

/// [`init_status_line`] with the keymap supplied — a seam so tests can drive
/// a configured keymap without mutating the process-global config path (which
/// a `OnceLock` would make irreversible for every other test in the binary).
pub(crate) fn init_status_line_with(r: Rect, kb: &Keybindings) -> Option<StatusLine> {
    let mut r = r;
    r.a.y = r.b.y - 1;
    let defs = StatusDef::list()
        .def_one_of([crate::views::containers::HELP_CTX], |d| {
            status_hidden(status_always(d, kb), kb)
                .item(
                    "~F2~ Logs",
                    k(&kb.open_logs, KeyEvent::from(Key::F(2))),
                    CMD_REQ_LOGS,
                )
                .item(
                    "~F4~ Shell",
                    k(&kb.open_shell, KeyEvent::from(Key::F(4))),
                    CMD_REQ_SHELL,
                )
                .key_item(
                    k(&kb.open_details, KeyEvent::from(Key::Enter)),
                    CMD_REQ_DETAILS,
                )
                .key_item(k(&kb.delete, KeyEvent::from(Key::Delete)), CMD_REQ_DELETE)
                .key_item(
                    KeyEvent::new(
                        Key::Char('e'),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    CMD_REQ_EXPLORER,
                )
                .key_item(
                    k(&kb.select_all, ctrl_chord(Key::Char('a'))),
                    CMD_SELECT_ALL,
                )
                .key_item(
                    KeyEvent::new(
                        Key::Char('o'),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    Command::TABLE_COLUMNS,
                )
        })
        // Images: visible F2 = Run…; hidden Del.
        .def_one_of([crate::views::images::HELP_CTX], |d| {
            status_hidden(status_always(d, kb), kb)
                .item(
                    "~F2~ Run…",
                    k(&kb.run_image, KeyEvent::from(Key::F(2))),
                    CMD_REQ_RUN,
                )
                .key_item(
                    k(&kb.remove_image, KeyEvent::from(Key::Delete)),
                    CMD_REQ_DELETE,
                )
                .key_item(
                    KeyEvent::new(
                        Key::Char('e'),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    CMD_REQ_IMAGE_EXPLORER,
                )
                .key_item(
                    KeyEvent::new(
                        Key::Char('l'),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    CMD_REQ_IMAGE_LAYERS,
                )
                .key_item(
                    KeyEvent::new(
                        Key::Char('o'),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    Command::TABLE_COLUMNS,
                )
        })
        // Networks: hidden Del; Ctrl+O.
        .def_one_of([crate::views::networks::HELP_CTX], |d| {
            status_hidden(status_always(d, kb), kb)
                .key_item(k(&kb.delete, KeyEvent::from(Key::Delete)), CMD_REQ_DELETE)
                .key_item(
                    KeyEvent::new(
                        Key::Char('o'),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    Command::TABLE_COLUMNS,
                )
        })
        // Volumes: Enter opens Explorer, Del, Ctrl+O.
        .def_one_of([crate::views::volumes::HELP_CTX], |d| {
            status_hidden(status_always(d, kb), kb)
                .key_item(KeyEvent::from(Key::Enter), CMD_REQ_VOLUME_EXPLORER)
                .key_item(k(&kb.delete, KeyEvent::from(Key::Delete)), CMD_REQ_DELETE)
                .key_item(
                    KeyEvent::new(
                        Key::Char('o'),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    Command::TABLE_COLUMNS,
                )
        })
        // Explorer: F5 Refresh, Del.
        .def_one_of([crate::views::explorer::HELP_CTX], |d| {
            status_hidden(status_always(d, kb), kb)
                .item(
                    "~F5~ Refresh",
                    KeyEvent::from(Key::F(5)),
                    crate::views::explorer::CMD_EXP_REFRESH,
                )
                .key_item(
                    k(&kb.delete, KeyEvent::from(Key::Delete)),
                    crate::views::explorer::CMD_EXP_DELETE,
                )
        })
        .def_one_of([crate::views::logs::HELP_CTX], |d| {
            status_hidden(status_always(d, kb), kb).item(
                "~F7~ Follow",
                KeyEvent::from(Key::F(7)),
                CMD_TOGGLE_FOLLOW,
            )
        })
        // Diagnostics: no F-key action. Binding Tab/Up/PageUp to
        // `Command::NEXT`/`PREV` would STEAL the keys from the focused
        // pane (a `key_item` pre-routes them — status_line's hidden
        // items still fire) and break §5's own keys. The window handles
        // them itself: the `Group` advances panes on Tab, and
        // `TextPane` scrolls on arrows/PgUp/PgDn/Home/End. So this def
        // only normalises the bar (hidden Alt-X + chords).
        .def_one_of([crate::views::diagnostics::HELP_CTX], |d| {
            status_hidden(status_always(d, kb), kb)
        })
        .def_all(|d| {
            status_always(d, kb)
                .item("~Alt-X~ Exit", k(&kb.quit, alt('x')), Command::QUIT)
                .key_item(
                    KeyEvent::new(
                        Key::F(5),
                        KeyModifiers {
                            ctrl: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    Command::RESIZE,
                )
                .key_item(
                    KeyEvent::new(
                        Key::F(6),
                        KeyModifiers {
                            shift: true,
                            ..KeyModifiers::default()
                        },
                    ),
                    Command::PREV,
                )
        })
        .build();
    Some(StatusLine::new(r, defs))
}

/// Menu bar : File / Docker / Windows / Options / Help — no disabled
/// stubs.
pub fn init_menu_bar(r: Rect) -> Option<Box<dyn View>> {
    let mut r = r;
    r.b.y = r.a.y + 1;
    let menu = Menu::builder()
        .submenu("~F~ile", alt('f'), |m| {
            m.command_key("~E~xit", Command::QUIT, alt('x'), "Alt-X")
        })
        .submenu("~D~ocker", alt('d'), |m| {
            m.command("~C~ontainers", CMD_OPEN_CONTAINERS)
                .command("~I~mages", CMD_OPEN_IMAGES)
                .command("~N~etworks", CMD_OPEN_NETWORKS)
                .command("~V~olumes", CMD_OPEN_VOLUMES)
                .command("~E~vents", CMD_OPEN_EVENTS)
                .command("~S~tatistics", CMD_OPEN_STATISTICS)
        })
        .submenu("~W~indows", alt('w'), |m| {
            m.command("~S~ize/Move", Command::RESIZE)
                .command("~Z~oom", Command::ZOOM)
                .command("~T~ile", Command::TILE)
                .command("~C~ascade", Command::CASCADE)
                .command("~N~ext", Command::NEXT)
                .command("~P~revious", Command::PREV)
                .command("~x~Close", Command::CLOSE)
        })
        // Settings (settings): one window, focused if already open.
        .submenu("~O~ptions", alt('o'), |m| {
            m.command("~T~hemes…", CMD_THEMES)
                .command("~E~dit colors…", CMD_THEME_EDITOR)
                .separator()
                .command("~S~ettings…", CMD_SETTINGS)
        })
        .submenu("~H~elp", alt('h'), |m| {
            m.command("~S~hortcuts", CMD_HELP)
                .command("~C~heck for updates…", CMD_CHECK_UPDATES)
                .command("~A~bout", CMD_ABOUT)
        })
        .build();
    Some(Box::new(MenuBar::new(r, menu)))
}

/// Options ▸ Themes… : run the picker modally, then — only on OK — install
/// the chosen palette and persist `theme = "…"` in the config file.
///
/// `run_app`'s closure is the one place that holds `&mut Program`, which is
/// where `set_theme` lives (a View cannot reach it — same reason
/// `Program::theme_editor()` is a `Program` method). The picker itself never
/// applies anything: it previews from the entry's own [`Theme`], so an Esc
/// needs no undo.
fn open_theme_picker(prog: &mut Program) {
    use crate::domain::theme;
    use crate::views::theme_picker::{ThemeEntry, ThemePickerDialog, picker_rect};

    let (cfg, _) = crate::domain::config::TvdockerConfig::load();
    let current = cfg.theme.clone().unwrap_or_else(|| "classic".into());
    let entries = theme::theme_names(&cfg.themes)
        .into_iter()
        .map(|name| {
            let summary = theme::summary_for(&cfg.themes, &name);
            ThemeEntry::new(name.clone(), theme::resolve(&cfg.themes, &name), summary)
        })
        .collect();
    let bounds = picker_rect(prog.desktop_rect());
    let picked = prog.exec_view_with(
        Box::new(ThemePickerDialog::new(bounds, entries, &current)),
        |modal, cmd| {
            if cmd == Command::OK {
                modal
                    .as_any_mut()
                    .and_then(|a| a.downcast_mut::<ThemePickerDialog>())
                    .and_then(|p| p.selected().map(str::to_string))
            } else {
                None
            }
        },
    );
    let Some(name) = picked else {
        return; // Esc: the active theme was never touched
    };
    prog.set_theme(theme::resolve(&cfg.themes, &name));
    if let Err(e) = crate::domain::config::TvdockerConfig::set_theme_in_file(
        &crate::domain::config::TvdockerConfig::path(),
        &name,
    ) {
        let _ = prog.message_box(
            &format!("Theme applied — but the config file could not be updated:\n{e}"),
            MessageBoxKind::Warning,
            MessageBoxButtons::ok(),
        );
    }
}

/// Options ▸ Edit colors… : edit a WORKING copy of the palette, install it,
/// then offer to keep it as a named theme (blank name = this session only).
///
/// tvision's own `Program::theme_editor()` would apply an edit too, but
/// `Program::theme` is private with no getter — there would be no way to read
/// the result back and write it to the config, which is why the editor itself
/// lives in `views/theme_editor.rs`.
fn edit_colors(prog: &mut Program) {
    use crate::domain::config::TvdockerConfig;
    use crate::domain::theme;
    use crate::views::theme_editor::{ThemeEditorDialog, editor_rect};

    let (cfg, _) = TvdockerConfig::load();
    let current = cfg.theme.clone().unwrap_or_else(|| "classic".into());
    let working = theme::resolve(&cfg.themes, &current);
    let bounds = editor_rect(prog.desktop_rect());
    let edited = prog.exec_view_with(
        Box::new(ThemeEditorDialog::new(bounds, working)),
        |modal, cmd| {
            if cmd == Command::OK {
                modal
                    .as_any_mut()
                    .and_then(|a| a.downcast_mut::<ThemeEditorDialog>())
                    .map(|d| d.theme())
            } else {
                None
            }
        },
    );
    let Some(edited) = edited else {
        return; // Esc: the live palette was never touched
    };
    prog.set_theme(edited.clone());

    // Keep it? An empty name leaves the palette for this session only. A
    // CUSTOM theme keeps its own name in the prompt (OK then re-saves the
    // edits in place); presets prompt blank so a stray OK can't silently
    // fork one.
    let prefill = if cfg.themes.contains_key(&current) {
        current.clone()
    } else {
        String::new()
    };
    let (cmd, name) = prog.input_box(
        "Save as theme",
        "Name (blank = this session only)",
        &prefill,
        32,
    );
    if cmd != Command::OK {
        return;
    }
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    let def = theme::theme_to_def(&edited);
    if let Err(e) = crate::domain::config::save_theme_as(&TvdockerConfig::path(), &name, &def) {
        let _ = prog.message_box(
            &format!("Palette applied — but it could not be saved:\n{e}"),
            MessageBoxKind::Warning,
            MessageBoxButtons::ok(),
        );
    }
}

/// `Program::new` with the plain status line. `desktop_fn` is `FnOnce`
/// (verified: `Program::new` takes
/// `impl FnOnce(Rect) -> Option<Box<dyn View>>`), so capturing desktop
/// closures are fine.
pub fn program_with(
    backend: Box<dyn tv::Backend>,
    clock: Box<dyn tv::Clock>,
    desktop_fn: impl FnOnce(Rect) -> Option<Box<dyn View>>,
) -> Program {
    program_with_status(backend, clock, desktop_fn, |r| {
        init_status_line(r).map(|s| Box::new(s) as Box<dyn View>)
    })
}

/// `Program::new` with an explicit status-line factory (the dynamic
/// status needs the shared core).
pub fn program_with_status(
    backend: Box<dyn tv::Backend>,
    clock: Box<dyn tv::Clock>,
    desktop_fn: impl FnOnce(Rect) -> Option<Box<dyn View>>,
    status_fn: impl FnOnce(Rect) -> Option<Box<dyn View>>,
) -> Program {
    // The theme comes from `theme =` / `[themes.*]` — same config read the
    // status line already does for `[keybindings]`. An unset or unknown
    // selection resolves to `classic`, the stock tvision palette.
    let (cfg, _) = crate::domain::config::TvdockerConfig::load();
    let selection = cfg.theme.as_deref().unwrap_or("classic");
    let theme = crate::domain::theme::resolve(&cfg.themes, selection);
    Program::new(backend, clock, theme, desktop_fn, status_fn, init_menu_bar)
}

/// Build the running program: dynamic status + Containers desktop +
/// bridge. `task_tx` is the worker's channel: `run`
/// creates the pair and spawns the worker; tests create a pair and
/// keep the receiver when they assert on `TaskReq`s. The returned
/// senders are the 3A pair (AppMsg mpsc + ActionReq broadcast).
pub fn build(
    backend: Box<dyn tv::Backend>,
    clock: Box<dyn tv::Clock>,
    core: Rc<RefCell<AppCore>>,
    filter: Option<&str>,
    task_tx: mpsc::UnboundedSender<TaskReq>,
) -> (Program, mpsc::Sender<AppMsg>, broadcast::Sender<ActionReq>) {
    let (app_tx, app_rx) = mpsc::channel::<AppMsg>(APP_CHANNEL_CAP);
    let (action_tx, _no_initial_receiver) = broadcast::channel::<ActionReq>(ACTION_CHANNEL_CAP);
    let program = program_with_status(
        backend,
        clock,
        {
            let core = Rc::clone(&core);
            let action_tx = action_tx.clone();
            move |r| {
                crate::views::desktop::containers_desktop(
                    r, core, filter, app_rx, action_tx, task_tx,
                )
            }
        },
        {
            let core = Rc::clone(&core);
            move |r| crate::views::status::dynamic_status(r, core)
        },
    );
    (program, app_tx, action_tx)
}

/// Spin the event loop until `Command::QUIT`; shows the optional startup
/// error (config parse) as a message box first. Handles the
/// app-level commands; probes reuse this so every example gets Help/About.
/// a background outcome goes to the STATUS LATCH, never a modal.
/// An error latches as `TaskError` (the visible ⚠), everything else as
/// `Status`, and `None` — a placeholder repository with nothing published —
/// says nothing at all. Pure, so the routing rule is testable without a
/// network round trip.
fn background_update_msg(result: &anyhow::Result<domain::update::Outcome>) -> Option<AppMsg> {
    domain::update::status_text(env!("CARGO_PKG_VERSION"), result).map(|text| {
        if result.is_err() {
            AppMsg::TaskError(text)
        } else {
            AppMsg::Status(text)
        }
    })
}

/// [`run_program_for`] with a throwaway install channel — enough for
/// the modal tests that never reach the install confirm (an install
/// request posted here evaporates: no worker owns the receiver).
/// Production and install-flow tests pass the real channel.
pub fn run_program(program: Program, startup_error: Option<String>) {
    let (task_tx, _dead) = mpsc::unbounded_channel::<TaskReq>();
    run_program_for(program, startup_error, &task_tx);
}

/// [`run_program_with`] with the running version and the real check.
pub fn run_program_for(
    program: Program,
    startup_error: Option<String>,
    task_tx: &mpsc::UnboundedSender<TaskReq>,
) {
    run_program_with(
        program,
        startup_error,
        task_tx,
        env!("CARGO_PKG_VERSION"),
        domain::update::check_for_update,
    )
}

/// [`run_program`] with the running version and the check injected —
/// the seam that lets the confirm-and-install flow be driven without a
/// repository or a network round trip.
pub fn run_program_with(
    program: Program,
    startup_error: Option<String>,
    task_tx: &mpsc::UnboundedSender<TaskReq>,
    current: &str,
    check: impl Fn(&str) -> anyhow::Result<domain::update::Outcome>,
) {
    let mut program = program;
    if let Some(err) = startup_error {
        let _ = program.message_box(&err, MessageBoxKind::Error, MessageBoxButtons::ok());
    }
    let _ = program.run_app(|prog, cmd| {
        if cmd == CMD_HELP {
            let _ = prog.message_box(
                SHORTCUTS_TEXT,
                MessageBoxKind::Information,
                MessageBoxButtons::ok(),
            );
        } else if cmd == CMD_ABOUT {
            let _ = prog.message_box(
                &about_text(),
                MessageBoxKind::Information,
                MessageBoxButtons::ok(),
            );
        } else if cmd == CMD_THEMES {
            // Options ▸ Themes… — a modal picker; Enter installs the chosen
            // palette AND persists `theme = "…"` (Esc changes nothing).
            open_theme_picker(prog);
        } else if cmd == CMD_THEME_EDITOR {
            // Options ▸ Edit colors… — author over a working copy, install it,
            // then offer to keep it as a named theme.
            edit_colors(prog);
        } else if cmd == CMD_CHECK_UPDATES {
            // The user asked, so the short blocking check runs here and
            // the outcome is a modal. A self-updatable release asks for
            // consent first; on Yes the WORKER does the download/swap
            // (blocking I/O off the UI thread) and reports through the
            // status latch. The startup check never takes this path —
            // it runs on the task pool and posts to the status latch
            // instead (no modal from a background).
            let result = check(current);
            let offer = match &result {
                Ok(domain::update::Outcome::Available(r)) => {
                    r.asset.clone().map(|url| (r.version.clone(), url))
                }
                _ => None,
            };
            if let Some((version, url)) = offer {
                let text = domain::update::describe_install_offer(current, &version);
                let answer = prog.message_box(
                    &text,
                    MessageBoxKind::Information,
                    MessageBoxButtons::yes_no(),
                );
                if answer == Command::YES {
                    let _ = task_tx.send(TaskReq::UpdateInstall { version, url });
                    return;
                }
            }
            let checked = domain::update::release_api_url();
            let text = domain::update::describe(current, &checked, &result);
            let _ = prog.message_box(&text, MessageBoxKind::Information, MessageBoxButtons::ok());
        }
    });
}

/// Run the TUI (entry point): config → channels → poller →
/// program → loop. The poll interval comes from the config (—
/// honored, not hardcoded) and the parse error surfaces as the §10
/// startup message box.
pub async fn run(filter: Option<String>) -> anyhow::Result<()> {
    let (mut config, parse_err) = domain::config::TvdockerConfig::load();
    config.polling.clamp();
    let core = Rc::new(RefCell::new(AppCore::new()));
    let backend = CrosstermBackend::new()?;
    let (task_tx, task_rx) = mpsc::unbounded_channel::<TaskReq>();
    let (program, app_tx, action_tx) = build(
        Box::new(backend),
        Box::new(SystemClock::new()),
        core,
        filter.as_deref(),
        task_tx.clone(),
    );
    // One worker for every Docker side effect the UI cannot do
    //; the six pollers follow.
    domain::docker::tasks::spawn_task_worker(app_tx.clone(), task_rx, action_tx.clone());
    // The containers poller OWNS connection state; the four list pollers
    // and the events stream are silent. The five POLLERS
    // subscribe to the SAME broadcast ⇒ opening any window polls all
    // lists; `spawn_events` takes no receiver — Docker
    // pushes that stream, it is never polled on request.
    domain::docker::poller::spawn_containers(
        app_tx.clone(),
        action_tx.subscribe(),
        config.polling.containers_ms,
    );
    domain::docker::poller::spawn_list(
        app_tx.clone(),
        action_tx.subscribe(),
        config.polling.images_ms,
        |d| async move { domain::docker::images::list_images(&d).await },
        AppMsg::Images,
    );
    domain::docker::poller::spawn_list(
        app_tx.clone(),
        action_tx.subscribe(),
        config.polling.networks_ms,
        |d| async move { domain::docker::networks::list_networks(&d).await },
        AppMsg::Networks,
    );
    domain::docker::poller::spawn_list(
        app_tx.clone(),
        action_tx.subscribe(),
        config.polling.volumes_ms,
        |d| async move { domain::docker::volumes::list_volumes(&d).await },
        AppMsg::Volumes,
    );
    domain::docker::poller::spawn_list(
        app_tx.clone(),
        action_tx.subscribe(),
        config.polling.statistics_ms,
        |d| async move { domain::docker::statistics::list_statistics(&d).await },
        AppMsg::Statistics,
    );
    // OPTIONAL background check: `check_updates = true` asks once
    // per start, OFF the UI thread (ureq blocks), and reports through the
    // §10 status latch. An unset or false key stays silent.
    if domain::update::check_on_startup(config.check_updates) {
        let tx = app_tx.clone();
        tokio::spawn(async move {
            let current = env!("CARGO_PKG_VERSION").to_string();
            let joined =
                tokio::task::spawn_blocking(move || domain::update::check_for_update(&current))
                    .await;
            let outcome = match joined {
                Ok(result) => result,
                Err(join) => Err(anyhow::anyhow!("update check: {join}")),
            };
            if let Some(msg) = background_update_msg(&outcome) {
                let _ = tx.send(msg).await;
            }
        });
    }
    domain::docker::poller::spawn_events(app_tx); // moves the last handle
    run_program_for(program, parse_err, &task_tx);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv::Event;
    use tv::HelpCtx;

    #[test]
    fn about_names_the_running_version() {
        let text = about_text();
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "the About box shows the running version: {text}"
        );
        assert!(text.contains("tvdocker"), "it still names the project");
    }

    /// Build the status line and drive ONE key through the real
    /// pre-route: returns the transformed event.
    ///
    /// The keymap is **pinned to the defaults**: these tests assert the
    /// stock keymap, and `init_status_line` reads the developer's real
    /// config on every call (dcargo mounts `~/.config/tvdocker` into the
    /// container) — a `[keybindings]` override in that file would turn
    /// Review Focus #3's guard red for anyone who uses the feature.
    fn transform(ctx: HelpCtx, ev: Event) -> Event {
        let mut line = init_status_line_with(Rect::new(0, 24, 80, 25), &Keybindings::default())
            .expect("status line");
        line.set_help_ctx(ctx);
        let mut ev = ev;
        crate::testutil::with_ctx(|c| line.handle_event(&mut ev, c));
        ev
    }

    fn key(k: Key) -> Event {
        Event::KeyDown(KeyEvent::from(k))
    }

    /// a background outcome goes to the STATUS LATCH, never a
    /// modal — and an error must latch as `TaskError`, not as a plain info
    /// line that reads like success.
    #[test]
    fn a_background_update_failure_latches_as_an_error() {
        let err = background_update_msg(&Err(anyhow::anyhow!("timeout")));
        assert!(
            matches!(err, Some(AppMsg::TaskError(_))),
            "an error latches as an error"
        );
        let ok = background_update_msg(&Ok(domain::update::Outcome::UpToDate));
        assert!(
            matches!(ok, Some(AppMsg::Status(_))),
            "success latches as info"
        );
        assert!(
            background_update_msg(&Ok(domain::update::Outcome::NoReleaseYet)).is_none(),
            "a placeholder repo that published nothing stays silent"
        );
    }

    #[test]
    fn containers_context_transforms_f2_f4_enter_del_and_ctrl_a() {
        use crate::views::containers::HELP_CTX;
        for (k, want) in [
            (Key::F(2), CMD_REQ_LOGS),
            (Key::F(4), CMD_REQ_SHELL),
            (Key::Enter, CMD_REQ_DETAILS),
            (Key::Delete, CMD_REQ_DELETE),
        ] {
            let out = transform(HELP_CTX, key(k));
            assert_eq!(out, Event::Command(want), "{k:?} is BOUND while hidden");
        }
        let ctrl_a = Event::KeyDown(KeyEvent::new(
            Key::Char('a'),
            KeyModifiers {
                ctrl: true,
                ..KeyModifiers::default()
            },
        ));
        assert_eq!(transform(HELP_CTX, ctrl_a), Event::Command(CMD_SELECT_ALL));
    }

    #[test]
    fn alt_x_and_enter_still_transform_while_a_window_is_focused() {
        // hidden ≠ unbound.
        use crate::views::containers::HELP_CTX;
        let alt_x = Event::KeyDown(alt('x'));
        assert_eq!(transform(HELP_CTX, alt_x), Event::Command(Command::QUIT));
        let f7_logs_window = transform(crate::views::logs::HELP_CTX, key(Key::F(7)));
        assert_eq!(f7_logs_window, Event::Command(CMD_TOGGLE_FOLLOW));
        // …while containers-only keys do NOT leak into other defs:
        let f2_on_logs = transform(crate::views::logs::HELP_CTX, key(Key::F(2)));
        assert_eq!(f2_on_logs, key(Key::F(2)), "no dead/wrong binding");
    }

    #[test]
    fn the_universal_def_leaves_enter_and_del_unbound() {
        // The modals (shell config) report NO_CONTEXT, so
        // while one is open the pump's idle pass re-targets the status
        // line to the MODAL's ctx (tvision-rs program.rs:2176-2198) and
        // this universal def applies. It must NOT bind Enter/Del — a
        // live keypress has to reach the dialog and submit it (the user
        // verified this: Enter in the shell config runs the shell). Only
        // the pre-queued test harness ever sees the stale Containers def,
        // because it never idles between events; that is why
        // `tests/resource_actions.rs` answers modals with commands.
        let out = transform(HelpCtx::NO_CONTEXT, key(Key::Enter));
        assert_eq!(out, key(Key::Enter), "Enter unbound in the universal def");
        let del = transform(HelpCtx::NO_CONTEXT, key(Key::Delete));
        assert_eq!(del, key(Key::Delete), "Del unbound too (§5 scopes it)");
    }

    #[test]
    fn universal_def_still_transforms_everything() {
        let out = transform(HelpCtx::NO_CONTEXT, key(Key::F(5)));
        assert_eq!(out, Event::Command(Command::ZOOM), "def_all intact");
        let alt_x = Event::KeyDown(alt('x'));
        assert_eq!(
            transform(HelpCtx::NO_CONTEXT, alt_x),
            Event::Command(Command::QUIT)
        );
    }

    #[test]
    fn f2_run_is_scoped_to_the_images_def() {
        use crate::views::{images, networks, volumes};
        assert_eq!(
            transform(images::HELP_CTX, key(Key::F(2))),
            Event::Command(CMD_REQ_RUN),
            "Images: F2 = Run…"
        );
        assert_eq!(
            transform(images::HELP_CTX, key(Key::Delete)),
            Event::Command(CMD_REQ_DELETE),
            "Images: Del = Remove (hidden-but-bound)"
        );
        for ctx in [networks::HELP_CTX, volumes::HELP_CTX] {
            assert_eq!(
                transform(ctx, key(Key::Delete)),
                Event::Command(CMD_REQ_DELETE),
                "Del bound"
            );
            assert_eq!(
                transform(ctx, key(Key::F(2))),
                key(Key::F(2)),
                "F2 must NOT leak from the Images def (no dead binding)"
            );
        }
    }

    /// `Ctrl+E` opens the Explorer from Containers and from Images — the
    /// direct keyboard route (the context menu, Shift+F10, goes through
    /// the same menu items).
    #[test]
    fn ctrl_e_opens_the_container_and_image_explorers() {
        use crate::views::{containers, images};
        let ctrl_e = Event::KeyDown(KeyEvent::new(
            Key::Char('e'),
            KeyModifiers {
                ctrl: true,
                ..KeyModifiers::default()
            },
        ));
        assert_eq!(
            transform(containers::HELP_CTX, ctrl_e.clone()),
            Event::Command(CMD_REQ_EXPLORER),
            "Ctrl+E in Containers"
        );
        assert_eq!(
            transform(images::HELP_CTX, ctrl_e),
            Event::Command(CMD_REQ_IMAGE_EXPLORER),
            "Ctrl+E in Images"
        );
    }

    /// `Ctrl+L` is the keyboard-only Layers route, bound in Images the way
    /// `Ctrl+E` is bound there for the Explorer.
    #[test]
    fn ctrl_l_opens_the_layers_window_from_images() {
        use crate::views::images;
        let ctrl_l = Event::KeyDown(KeyEvent::new(
            Key::Char('l'),
            KeyModifiers {
                ctrl: true,
                ..KeyModifiers::default()
            },
        ));
        assert_eq!(
            transform(images::HELP_CTX, ctrl_l),
            Event::Command(CMD_REQ_IMAGE_LAYERS),
            "Ctrl+L in Images"
        );
    }

    /// The F1 box documents what the STATUS BAR cannot: hidden chords and
    /// window-local keys. It must not repeat the always-visible bar — that
    /// text is already on screen, and dropping it frees the room for chords
    /// like `Ctrl+E`, which no bar can show.
    #[test]
    fn shortcuts_document_the_hidden_bindings_not_the_status_bar() {
        assert!(
            SHORTCUTS_TEXT.contains("Ctrl+E Explorer"),
            "the keyboard-only explorer route is documented: {SHORTCUTS_TEXT}"
        );
        assert!(
            SHORTCUTS_TEXT.contains("Ctrl+L Layers"),
            "the keyboard-only layers route is documented: {SHORTCUTS_TEXT}"
        );
        assert!(
            SHORTCUTS_TEXT.contains("Shift+F10"),
            "the mouse-free context menu is documented: {SHORTCUTS_TEXT}"
        );
        for visible in [
            "F1 Help",
            "F3 Close",
            "F5 Zoom",
            "F6 Next",
            "F10 Menu",
            "Alt-X Exit",
            // …and the menu bar, likewise permanently on screen: its Alt
            // chords are the bar's own hot letters, and the Windows submenu
            // opens on demand.
            "Alt-F File",
            "Alt-D Docker",
            "Alt-W Windows",
            "Alt-H Help",
            "Size/Move",
        ] {
            assert!(
                !SHORTCUTS_TEXT.contains(visible),
                "{visible:?} is on the status bar already — not repeated here"
            );
        }
        // Everything that was documented stays documented.
        for keeps in [
            "←/→ cycle the sort column",
            "Shift+←/→",
            "Ctrl-A selects all",
            "Enter Explorer",
            // …and the menu entries that no status bar shows:
            "Options → Settings…",
            "Diagnose…",
        ] {
            assert!(SHORTCUTS_TEXT.contains(keeps), "keeps {keeps:?}");
        }
    }
    /// An override REPLACES the default (settings): F8 takes over
    /// Logs and F2 stops transforming, while the visible label still reads
    /// `~F2~ Logs` — so the bar's 47-cell layout never shifts.
    ///
    /// Built through the config seam, so the process-global config path —
    /// which a `OnceLock` makes irreversible and every other test in this
    /// binary reads — is never touched.
    #[test]
    fn an_override_replaces_the_default_chord() {
        let kb = Keybindings {
            open_logs: vec!["F8".to_string()],
            ..Keybindings::default()
        };
        let mut line = init_status_line_with(Rect::new(0, 24, 80, 25), &kb).expect("status line");
        line.set_help_ctx(crate::views::containers::HELP_CTX);
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut ctx = tv::Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::KeyDown(KeyEvent::from(Key::F(8)));
            line.handle_event(&mut ev, &mut ctx);
            assert_eq!(ev, Event::Command(CMD_REQ_LOGS), "F8 opens Logs");

            let mut ev2 = Event::KeyDown(KeyEvent::from(Key::F(2)));
            line.handle_event(&mut ev2, &mut ctx);
            assert_ne!(ev2, Event::Command(CMD_REQ_LOGS), "F2 no longer does");
        }
    }

    /// Review Focus #2 (settings): a NON-rebindable value is
    /// rejected and the field FALLS BACK TO THE DEFAULT — `quit = "Tab"`
    /// must not steal Tab, and Alt-X must still quit.
    #[test]
    fn a_navigation_primitive_override_is_reported_and_the_default_survives() {
        let kb = Keybindings {
            quit: vec!["Tab".into()],
            ..Keybindings::default()
        };
        // Reported…
        let rejected = crate::domain::config::validate_keybindings(&kb);
        assert!(
            rejected.iter().any(|(f, _)| f == "keybindings.quit"),
            "the field is named: {rejected:?}"
        );
        // …and the default binding still works.
        assert_default_quit_still_works(kb);
        // Same for an override that does not parse at all: the DEFAULT
        // chord must survive, modifiers included.
        assert_default_quit_still_works(Keybindings {
            quit: vec!["banana".to_string()],
            ..Keybindings::default()
        });
    }

    /// The default `quit` chord stands when the configured one is unusable.
    fn assert_default_quit_still_works(kb: Keybindings) {
        let mut line = init_status_line_with(Rect::new(0, 24, 80, 25), &kb).expect("status line");
        line.set_help_ctx(crate::views::containers::HELP_CTX);
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut ctx = tv::Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::KeyDown(KeyEvent::from(Key::Tab));
            line.handle_event(&mut ev, &mut ctx);
            assert_ne!(
                ev,
                Event::Command(Command::QUIT),
                "a rejected Tab must NOT quit"
            );

            let mut ev2 = Event::KeyDown(alt('x'));
            line.handle_event(&mut ev2, &mut ctx);
            assert_eq!(ev2, Event::Command(Command::QUIT), "Alt-X still quits");

            let mut ev3 = Event::KeyDown(KeyEvent::from(Key::Char('x')));
            line.handle_event(&mut ev3, &mut ctx);
            assert_ne!(
                ev3,
                Event::Command(Command::QUIT),
                "a stripped modifier must never make a bare letter quit"
            );
        }
    }
}
