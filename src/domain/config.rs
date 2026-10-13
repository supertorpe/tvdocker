//! Config : schema ported from tvdocker.toml, renamed.
//! `keymap()` and its input-module dependency were NOT ported — the
//! `[keybindings]` overrides are tvdocker's own (settings).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tv::{Key, KeyEvent, KeyModifiers};

/// Accept either a bare string or a list of strings (ported verbatim —
/// tvdocker config.rs:5-28).
fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    struct StringOrVec;
    impl<'de> de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("string or list of strings")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> Result<Vec<String>, E> {
            Ok(vec![value.to_string()])
        }
        fn visit_seq<S: de::SeqAccess<'de>>(self, mut seq: S) -> Result<Vec<String>, S::Error> {
            let mut vec = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                vec.push(s);
            }
            Ok(vec)
        }
    }
    deserializer.deserialize_any(StringOrVec)
}

// Per-field defaults so a PARTIAL [polling] section still parses (the
// vendored source relied on save() writing whole sections; a hand-edited
// file must not silently fall back to full defaults).
fn default_containers_ms() -> u64 {
    2000
}
fn default_statistics_ms() -> u64 {
    2000
}
fn default_images_ms() -> u64 {
    10000
}
fn default_networks_ms() -> u64 {
    10000
}
fn default_volumes_ms() -> u64 {
    10000
}

/// Poll intervals in milliseconds (ported; defaults per).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PollingIntervals {
    #[serde(default = "default_containers_ms")]
    pub containers_ms: u64,
    #[serde(default = "default_statistics_ms")]
    pub statistics_ms: u64,
    #[serde(default = "default_images_ms")]
    pub images_ms: u64,
    #[serde(default = "default_networks_ms")]
    pub networks_ms: u64,
    #[serde(default = "default_volumes_ms")]
    pub volumes_ms: u64,
}

impl PollingIntervals {
    pub const MIN_MS: u64 = 500;
    pub const MAX_MS: u64 = 60_000;

    fn clamp_one(v: &mut u64) {
        *v = (*v).clamp(Self::MIN_MS, Self::MAX_MS);
    }

    /// Clamp every interval into [MIN_MS, MAX_MS] — a config of 0 ms must
    /// never become a spin loop (Review Focus #2).
    pub fn clamp(&mut self) {
        Self::clamp_one(&mut self.containers_ms);
        Self::clamp_one(&mut self.statistics_ms);
        Self::clamp_one(&mut self.images_ms);
        Self::clamp_one(&mut self.networks_ms);
        Self::clamp_one(&mut self.volumes_ms);
    }
}

impl Default for PollingIntervals {
    fn default() -> Self {
        Self {
            containers_ms: 2000,
            statistics_ms: 2000,
            images_ms: 10000,
            networks_ms: 10000,
            volumes_ms: 10000,
        }
    }
}

/// Per-container shell config (`[containers.<name>]`).
/// `PartialEq` supports `ShellForm::save_if_changed`'s unchanged-check.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContainerShellConfig {
    pub shell: Option<String>,
    pub user: Option<String>,
    pub workdir: Option<String>,
}

/// Column visibility per table (shown by the column pickers). All default
/// to visible.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContainerColumns {
    pub show_name: bool,
    pub show_image: bool,
    pub show_state: bool,
    pub show_ports: bool,
}

impl Default for ContainerColumns {
    fn default() -> Self {
        Self {
            show_name: true,
            show_image: true,
            show_state: true,
            show_ports: true,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ImageColumns {
    pub show_repository: bool,
    pub show_tag: bool,
    pub show_id: bool,
    pub show_size: bool,
}

impl Default for ImageColumns {
    fn default() -> Self {
        Self {
            show_repository: true,
            show_tag: true,
            show_id: true,
            show_size: true,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NetworkColumns {
    pub show_name: bool,
    pub show_id: bool,
    pub show_driver: bool,
    pub show_scope: bool,
    pub show_ipam: bool,
}

impl Default for NetworkColumns {
    fn default() -> Self {
        Self {
            show_name: true,
            show_id: true,
            show_driver: true,
            show_scope: true,
            show_ipam: true,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VolumeColumns {
    pub show_name: bool,
    pub show_driver: bool,
    pub show_mountpoint: bool,
    pub show_size: bool,
}

impl Default for VolumeColumns {
    fn default() -> Self {
        Self {
            show_name: true,
            show_driver: true,
            show_mountpoint: true,
            show_size: true,
        }
    }
}

/// No override — the TV-conventional binding applies
/// (overrides are OPTIONAL and scoped to status hotspots).
fn no_override() -> Vec<String> {
    Vec::new()
}

/// Keybinding overrides — schema ported verbatim (field names + the
/// string-or-vec deserializer); every default is empty because tvdocker's
/// defaults are the framework's TV conventions, not oh-my-docker's vim keys
/// (decision: dropped bindings). Values are written by the Settings window
/// (`Options → Settings…`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Keybindings {
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub quit: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub back: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub help: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub navigate_down: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub navigate_up: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub open_details: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub open_logs: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub open_shell: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub start_stop: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub restart: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub delete: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub search: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub toggle_selection: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub select_all: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub navigate_images: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub run_image: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub remove_image: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub remove_dangling_images: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub prune_images: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub jump_top: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub jump_bottom: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub sort_direction: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub logs_export: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub toggle_timestamps: Vec<String>,
    #[serde(default = "no_override", deserialize_with = "string_or_vec")]
    pub open_diagnostics: Vec<String>,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            quit: no_override(),
            back: no_override(),
            help: no_override(),
            navigate_down: no_override(),
            navigate_up: no_override(),
            open_details: no_override(),
            open_logs: no_override(),
            open_shell: no_override(),
            start_stop: no_override(),
            restart: no_override(),
            delete: no_override(),
            search: no_override(),
            toggle_selection: no_override(),
            select_all: no_override(),
            navigate_images: no_override(),
            run_image: no_override(),
            remove_image: no_override(),
            remove_dangling_images: no_override(),
            prune_images: no_override(),
            jump_top: no_override(),
            jump_bottom: no_override(),
            sort_direction: no_override(),
            logs_export: no_override(),
            toggle_timestamps: no_override(),
            open_diagnostics: no_override(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn llm_provider() -> String {
    "ollama".to_string()
}

fn llm_model() -> String {
    "llama3".to_string()
}

/// LLM diagnostics settings  — edited as TOML in the Settings
/// window; there is deliberately no dedicated form (settings).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LlmConfig {
    #[serde(default = "llm_provider")]
    pub provider: String,
    #[serde(default = "llm_model")]
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: llm_provider(),
            model: llm_model(),
            base_url: None,
            api_key: None,
        }
    }
}

/// The tvdocker config file (schema ported; renamed from `tvdockerConfig`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TvdockerConfig {
    pub latest_shell: Option<String>,
    #[serde(default)]
    pub containers: HashMap<String, ContainerShellConfig>,
    #[serde(default)]
    pub images: HashMap<String, ContainerShellConfig>,
    #[serde(default)]
    pub check_updates: Option<bool>,
    #[serde(default)]
    pub keybindings: Keybindings,
    #[serde(default)]
    pub polling: PollingIntervals,
    #[serde(default)]
    pub container_columns: ContainerColumns,
    #[serde(default)]
    pub image_columns: ImageColumns,
    #[serde(default)]
    pub network_columns: NetworkColumns,
    #[serde(default)]
    pub volume_columns: VolumeColumns,
    #[serde(default = "default_true")]
    pub mouse: bool,
    #[serde(default)]
    pub llm: Option<LlmConfig>,
}

impl Default for TvdockerConfig {
    fn default() -> Self {
        Self {
            latest_shell: Some("bash".into()),
            containers: Default::default(),
            images: Default::default(),
            check_updates: None,
            keybindings: Default::default(),
            polling: Default::default(),
            container_columns: Default::default(),
            image_columns: Default::default(),
            network_columns: Default::default(),
            volume_columns: Default::default(),
            mouse: true,
            llm: None,
        }
    }
}

/// Process-wide config-file override; unset in the running app.
///
/// The integration suite drives the REAL app, so it cannot retarget the
/// config with `env::set_var("HOME")` — edition 2024 makes that `unsafe`,
/// and this crate `forbid`s unsafe code. Instead each test process points
/// the config at a temp file via [`TvdockerConfig::set_path_override`]
/// before building the app, so the suite never reads or writes the
/// developer's real `~/.config/tvdocker/`. First call wins.
static CONFIG_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

impl TvdockerConfig {
    /// Retarget this process's config file at `path` (first call wins).
    /// The integration suite's hermeticity seam; the running app never
    /// calls it. See `CONFIG_PATH_OVERRIDE`.
    pub fn set_path_override(path: PathBuf) {
        let _ = CONFIG_PATH_OVERRIDE.set(path);
    }

    /// `$HOME/.config/tvdocker/tvdocker.toml` (— renamed from
    /// tvdocker.toml), or the [`TvdockerConfig::set_path_override`] target.
    pub fn path() -> PathBuf {
        if let Some(p) = CONFIG_PATH_OVERRIDE.get() {
            return p.clone();
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("tvdocker")
            .join("tvdocker.toml")
    }

    /// Load the user's config; `(defaults, None)` when the file is absent,
    /// `(defaults, Some(err))` when it fails to parse (surfaced as
    /// a startup message box — Task 7 wires that).
    /// The tuple is the contract: `(config, None)` = absent or clean,
    /// `(config, Some(err))` = parsed, then failed to parse. Callers that
    /// only need the config take `.0` (and knowingly drop the error — the
    /// startup path surfaces it instead,).
    pub fn load() -> (Self, Option<String>) {
        Self::load_from(&Self::path())
    }

    /// `load` against an explicit path (tests use temp files; edition 2024
    /// makes env-var mutation `unsafe`, so no HOME juggling).
    pub fn load_from(path: &Path) -> (Self, Option<String>) {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return (Self::default(), None),
        };
        match toml::from_str::<Self>(&content) {
            Ok(config) => (config, None),
            Err(e) => (Self::default(), Some(format!("Config parse error: {e}"))),
        }
    }

    /// Serialize this config to TOML. The Settings window's save path and its
    /// tests share this one serializer (settings).
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Write the config (kept with the schema; the Settings window saves
    /// through the editor's own `save_file`, this is the programmatic path).
    /// `Err(String)` rather than `anyhow` because the message is shown to
    /// the user verbatim by the callers (`save_if_changed` above), which
    /// return `String` themselves.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = self.to_toml().map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }
}

/// Parse one `[keybindings]` string into a `KeyEvent` (settings).
/// Case-insensitive, `-`-separated (a `+` also works): `Ctrl-E`, `Alt-X`,
/// `F2`, `Enter`, `a`.
pub fn parse_key(spec: &str) -> Option<KeyEvent> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut base: Option<&str> = None;
    for part in spec.split(['-', '+']) {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            _ if base.is_some() => return None, // two base keys
            _ => base = Some(part),
        }
    }
    let base = base?;
    let key = match base.to_ascii_lowercase().as_str() {
        "f1" => Key::F(1),
        "f2" => Key::F(2),
        "f3" => Key::F(3),
        "f4" => Key::F(4),
        "f5" => Key::F(5),
        "f6" => Key::F(6),
        "f7" => Key::F(7),
        "f8" => Key::F(8),
        "f9" => Key::F(9),
        "f10" => Key::F(10),
        "f11" => Key::F(11),
        "f12" => Key::F(12),
        "enter" | "return" => Key::Enter,
        "esc" | "escape" => Key::Esc,
        "tab" => Key::Tab,
        "backspace" | "back" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pgup" | "pageup" => Key::PageUp,
        "pgdn" | "pagedown" => Key::PageDown,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "space" => Key::Char(' '),
        _ if base.chars().count() == 1 => {
            // A modifier lowercases the base, because that is what the app
            // binds itself (`Ctrl-A` is `Key::Char('a')`) and what a terminal
            // delivers; a bare letter keeps its case.
            let mut c = base.chars().next()?;
            if ctrl || alt {
                c = c.to_ascii_lowercase();
            }
            Key::Char(c)
        }
        _ => return None,
    };
    let modifiers = KeyModifiers { ctrl, alt, shift };
    Some(KeyEvent::new(key, modifiers))
}

/// Resolve one binding (settings): an override list replaces the
/// default entirely — an empty list means "no override". Strings that do not
/// parse are collected so the save handler can report them rather than drop
/// them silently.
pub fn key_events(configured: &[String], default: &[Key]) -> (Vec<KeyEvent>, Vec<String>) {
    if configured.is_empty() {
        return (
            default.iter().map(|k| KeyEvent::from(*k)).collect(),
            Vec::new(),
        );
    }
    let mut events = Vec::new();
    let mut bad = Vec::new();
    for spec in configured {
        match parse_key(spec) {
            Some(ev) => events.push(ev),
            None => bad.push(spec.clone()),
        }
    }
    if events.is_empty() {
        // Every override was junk: keep the action reachable.
        events = default.iter().map(|k| KeyEvent::from(*k)).collect();
    }
    (events, bad)
}

/// Every override field by name.
fn keybinding_values(kb: &Keybindings) -> Vec<(&'static str, &[String])> {
    vec![
        ("quit", &kb.quit),
        ("back", &kb.back),
        ("help", &kb.help),
        ("navigate_down", &kb.navigate_down),
        ("navigate_up", &kb.navigate_up),
        ("open_details", &kb.open_details),
        ("open_logs", &kb.open_logs),
        ("open_shell", &kb.open_shell),
        ("start_stop", &kb.start_stop),
        ("restart", &kb.restart),
        ("delete", &kb.delete),
        ("search", &kb.search),
        ("toggle_selection", &kb.toggle_selection),
        ("select_all", &kb.select_all),
        ("navigate_images", &kb.navigate_images),
        ("run_image", &kb.run_image),
        ("remove_image", &kb.remove_image),
        ("remove_dangling_images", &kb.remove_dangling_images),
        ("prune_images", &kb.prune_images),
        ("jump_top", &kb.jump_top),
        ("jump_bottom", &kb.jump_bottom),
        ("sort_direction", &kb.sort_direction),
        ("logs_export", &kb.logs_export),
        ("toggle_timestamps", &kb.toggle_timestamps),
        ("open_diagnostics", &kb.open_diagnostics),
    ]
}

/// Why a key is fixed, or `None` when it may be rebound (settings).
/// Unparsable strings are not reported here — `key_events` owns that.
fn fixed_key_reason(spec: &str) -> Option<&'static str> {
    let ev = parse_key(spec)?;
    if matches!(
        ev.key,
        Key::Tab
            | Key::Esc
            | Key::Enter
            | Key::Backspace
            | Key::Char(' ')
            | Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
    ) {
        return Some("TV navigation primitive");
    }
    if matches!(ev.key, Key::F(10)) {
        return Some("the menu bar");
    }
    if ev.modifiers.alt {
        return Some("a menu accelerator");
    }
    None
}

/// Whether §7.4 lets this value be bound at all — `false` for the fixed keys.
/// [`validate_keybindings`] reports on this rule and the binding resolution
/// filters on it, so a value the save handler rejects can never take effect
/// ("the field falls back to its default"). An unparsable string is
/// not "fixed" — `key_events` owns it and falls back for that case too.
pub fn rebindable(spec: &str) -> bool {
    fixed_key_reason(spec).is_none()
}

/// The overrides §11 forbids, as `(field, value)` pairs (settings).
/// Enforced in one place so the policy can never drift from the schema.
pub fn validate_keybindings(kb: &Keybindings) -> Vec<(String, String)> {
    let mut rejected = Vec::new();
    for (field, values) in keybinding_values(kb) {
        for value in values {
            if let Some(reason) = fixed_key_reason(value) {
                rejected.push((
                    format!("keybindings.{field}"),
                    format!("{value} ({reason} is non-rebindable)"),
                ));
            }
        }
    }
    rejected
}

/// The override values that are not key strings at all, as `(field, value)`
/// pairs (settings row three). [`validate_keybindings`] owns the
/// *fixed*-key policy (§7.4); this owns "does it parse". The save handler
/// reports both, so a typo never silently does nothing (§7.2).
pub fn unparsable_keybindings(kb: &Keybindings) -> Vec<(String, String)> {
    let mut bad = Vec::new();
    for (field, values) in keybinding_values(kb) {
        for value in values {
            if parse_key(value).is_none() {
                bad.push((format!("keybindings.{field}"), value.clone()));
            }
        }
    }
    bad
}

/// The fields `init_status_line` actually emits an entry for (settings spec
/// §7.1 — the other 16 round-trip through the editor but wire nothing yet).
const WIRED_FIELDS: [&str; 9] = [
    "quit",
    "help",
    "open_details",
    "open_logs",
    "open_shell",
    "delete",
    "select_all",
    "run_image",
    "remove_image",
];

/// What the save handler must TELL the user beyond success/failure
/// an override with no wired action, a field whose
/// surplus chords are dropped (only the FIRST binds — see `app::k`), and the
/// chord each override actually bound — so a saved override never silently
/// does less than the report implies, and the bar's default label is not the
/// only place the user can learn the effective key. Empty for a stock config,
/// so a user with no overrides still gets a bare "Settings saved".
pub fn keybinding_notes(kb: &Keybindings) -> Vec<String> {
    let mut notes = Vec::new();
    for (field, values) in keybinding_values(kb) {
        if values.is_empty() {
            continue;
        }
        if !WIRED_FIELDS.contains(&field) {
            notes.push(format!(
                "keybindings.{field} = {} (accepted, no action yet)",
                values.join(", ")
            ));
            continue;
        }
        let usable: Vec<&String> = values
            .iter()
            .filter(|v| rebindable(v) && parse_key(v).is_some())
            .collect();
        if let Some(v) = usable.first() {
            notes.push(format!("keybindings.{field} → {v}"));
        }
        if usable.len() > 1 {
            notes.push(format!("keybindings.{field}: only the first chord binds"));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpfile(contents: &str) -> std::path::PathBuf {
        // No tempfile dep: std-only unique path under the system temp.
        let path = std::env::temp_dir().join(format!(
            "tvdocker-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn defaults_are_the_spec_values() {
        let c = TvdockerConfig::default();
        assert_eq!(c.polling.containers_ms, 2000);
        assert_eq!(c.polling.statistics_ms, 2000);
        assert_eq!(c.polling.images_ms, 10000);
        assert!(c.mouse, "mouse enabled by default");
        assert!(c.container_columns.show_name, "columns default to visible");
    }

    #[test]
    fn clamp_bounds_for_pathological_values() {
        let mut c = TvdockerConfig::default();
        c.polling.containers_ms = 0; // a person must not get a spin loop
        c.polling.images_ms = 10_000_000;
        c.polling.clamp();
        assert_eq!(c.polling.containers_ms, 500);
        assert_eq!(c.polling.images_ms, 60000);
    }

    #[test]
    fn load_from_reads_a_valid_file() {
        let path = tmpfile("[polling]\ncontainers_ms = 1500\n");
        let (c, err) = TvdockerConfig::load_from(&path);
        assert_eq!(err, None);
        assert_eq!(c.polling.containers_ms, 1500);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn load_from_reports_parse_errors_and_falls_back() {
        let path = tmpfile("this is not = toml [[[\n");
        let (c, err) = TvdockerConfig::load_from(&path);
        let err = err.expect("parse error surfaced");
        assert!(err.contains("Config parse error"), "got: {err}");
        assert_eq!(c.polling.containers_ms, 2000, "defaults retained");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn missing_file_yields_defaults_without_error() {
        let (c, err) =
            TvdockerConfig::load_from(std::path::Path::new("/nonexistent/tvdocker.toml"));
        assert_eq!(err, None);
        assert_eq!(c.polling.containers_ms, 2000);
    }

    #[test]
    fn path_points_at_the_tvdocker_config_file() {
        let p = TvdockerConfig::path();
        assert!(p.ends_with(".config/tvdocker/tvdocker.toml"), "{p:?}");
    }

    #[test]
    fn keybinding_defaults_are_empty_overrides() {
        // absent/empty = TV-conventional bindings apply;
        // the file only carries OVERRIDES (the Settings window writes them).
        let kb = Keybindings::default();
        assert!(kb.quit.is_empty() && kb.navigate_down.is_empty());
        // string-or-vec deserialization keeps working:
        let path = tmpfile("[keybindings]\nquit = \"Alt-X\"\n");
        let (c, err) = TvdockerConfig::load_from(&path);
        assert_eq!(err, None);
        assert_eq!(c.keybindings.quit, vec!["Alt-X".to_string()]);
        std::fs::remove_file(path).ok();
    }
    #[test]
    fn parse_key_reads_every_grammar_form() {
        use tv::{Key, KeyModifiers};
        let plain = parse_key("a").expect("plain char");
        assert_eq!(plain.key, Key::Char('a'));
        assert_eq!(plain.modifiers, KeyModifiers::default());

        // A modifier lowercases the base: the app binds Ctrl-A as Char('a').
        let ctrl = parse_key("Ctrl-E").expect("ctrl chord");
        assert_eq!(ctrl.key, Key::Char('e'));
        assert!(ctrl.modifiers.ctrl);

        assert_eq!(parse_key("f2").expect("fn key").key, Key::F(2));
        assert_eq!(parse_key("F12").expect("fn key").key, Key::F(12));
        assert_eq!(parse_key("Enter").expect("enter").key, Key::Enter);
        assert_eq!(parse_key("Alt-X").expect("alt chord").key, Key::Char('x'));
        assert!(parse_key("Alt-X").expect("alt chord").modifiers.alt);
        assert!(parse_key("Ctrl-Alt-Shift-q").is_some(), "modifiers combine");
        assert_eq!(parse_key("PgUp").expect("page up").key, Key::PageUp);
        assert_eq!(parse_key("space").expect("space").key, Key::Char(' '));
    }

    #[test]
    fn parse_key_rejects_junk() {
        assert!(parse_key("").is_none(), "empty");
        assert!(parse_key("Ctrl-").is_none(), "modifier with no key");
        assert!(parse_key("Ctrl-a-b").is_none(), "two base keys");
        assert!(parse_key("F13").is_none(), "no such function key");
        assert!(parse_key("banana").is_none(), "no such named key");
    }

    #[test]
    fn no_override_means_the_default_stands() {
        use tv::Key;
        let (events, bad) = key_events(&[], &[Key::F(2)]);
        assert_eq!(events.len(), 1, "one default binding");
        assert_eq!(events[0].key, Key::F(2));
        assert!(bad.is_empty(), "nothing to report");
    }

    /// The RESOLVER's contract (§7.2): a configured list replaces the
    /// default entirely, and every configured string that parses is resolved
    /// — in order. Note what this does NOT claim: `app::k` (the only binding
    /// site) takes the first resolved chord, so 2nd+ chords bind nothing.
    /// `keybinding_notes` reports that limitation on save rather than here.
    #[test]
    fn an_override_replaces_the_default_entirely() {
        use tv::Key;
        let (events, bad) = key_events(&["F9".into(), "Ctrl-L".into()], &[Key::F(2)]);
        assert_eq!(
            events.iter().map(|e| e.key).collect::<Vec<_>>(),
            vec![Key::F(9), Key::Char('l')],
            "both overrides, in order"
        );
        assert!(bad.is_empty());
    }

    #[test]
    fn key_events_collects_unparsable_strings() {
        use tv::Key;
        let (events, bad) = key_events(&["F9".into(), "banana".into()], &[Key::F(2)]);
        assert_eq!(events.len(), 1, "the parsable one survives");
        assert_eq!(bad, vec!["banana".to_string()], "reported");
    }

    #[test]
    fn every_override_unparsable_falls_back_to_the_default() {
        use tv::Key;
        let (events, bad) = key_events(&["banana".into()], &[Key::F(2)]);
        assert_eq!(events[0].key, Key::F(2), "the action stays reachable");
        assert_eq!(bad, vec!["banana".to_string()]);
    }

    #[test]
    fn validate_keybindings_rejects_the_fixed_keys() {
        let kb = Keybindings {
            quit: vec!["Tab".into()],
            help: vec!["F10".into()],
            delete: vec!["Alt-D".into()],
            open_logs: vec!["Esc".into()],
            run_image: vec!["Up".into()],
            ..Keybindings::default()
        };
        let rejected = validate_keybindings(&kb);
        let fields: Vec<&str> = rejected.iter().map(|(f, _)| f.as_str()).collect();
        for want in [
            "keybindings.quit",
            "keybindings.help",
            "keybindings.delete",
            "keybindings.open_logs",
            "keybindings.run_image",
        ] {
            assert!(fields.contains(&want), "{want} rejected: {rejected:?}");
        }
    }

    #[test]
    fn validate_keybindings_accepts_a_sane_override() {
        let kb = Keybindings {
            quit: vec!["Ctrl-Q".into()],
            open_logs: vec!["F8".into()],
            ..Keybindings::default()
        };
        let rejected = validate_keybindings(&kb);
        assert!(rejected.is_empty(), "{rejected:?}");
    }
    #[test]
    fn a_keybindings_table_deserializes_from_toml() {
        let parsed = toml::from_str::<TvdockerConfig>(
            "latest_shell = \"bash\"\n\n[keybindings]\nopen_logs = [\"F2\", \"F8\"]\n",
        )
        .expect("the minimal file parses");
        assert_eq!(
            parsed.keybindings.open_logs,
            vec!["F2".to_string(), "F8".to_string()],
            "the override list arrives"
        );
    }

    #[test]
    fn to_toml_round_trips_the_schema() {
        let text = TvdockerConfig::default().to_toml().expect("serialize");
        let back: TvdockerConfig = toml::from_str(&text).expect("parse back");
        assert_eq!(back.polling.images_ms, 10_000);
        assert!(back.mouse, "mouse enabled by default");
        assert_eq!(back.latest_shell.as_deref(), Some("bash"));
    }

    #[test]
    fn unparsable_keybindings_names_every_bad_value() {
        let kb = Keybindings {
            quit: vec!["Ctrl-Q".into(), "banana".into()],
            open_logs: vec!["nope!".into()],
            ..Keybindings::default()
        };
        assert_eq!(
            unparsable_keybindings(&kb),
            vec![
                ("keybindings.quit".to_string(), "banana".to_string()),
                ("keybindings.open_logs".to_string(), "nope!".to_string()),
            ],
            "a parsable sibling is not reported"
        );
    }

    #[test]
    fn keybinding_notes_is_empty_for_a_stock_config() {
        assert!(
            keybinding_notes(&Keybindings::default()).is_empty(),
            "a user with no overrides keeps a bare \"Settings saved\""
        );
    }

    /// Review #2/#3/#4: the save must not imply more than it delivered — an
    /// unwired field, a dropped surplus chord, and the chord that DID bind.
    #[test]
    fn keybinding_notes_discloses_unwired_surplus_and_effective_chords() {
        let kb = Keybindings {
            quit: vec!["Ctrl-Q".into(), "Ctrl-J".into()],
            search: vec!["/".into()],
            open_logs: vec!["F8".into()],
            ..Keybindings::default()
        };
        let notes = keybinding_notes(&kb);
        assert!(
            notes.contains(&"keybindings.quit → Ctrl-Q".to_string()),
            "the effective chord is named: {notes:?}"
        );
        assert!(
            notes.contains(&"keybindings.quit: only the first chord binds".to_string()),
            "the dropped chord is disclosed: {notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("keybindings.search") && n.contains("accepted, no action yet")),
            "an unwired field is disclosed: {notes:?}"
        );
        assert!(
            notes.contains(&"keybindings.open_logs → F8".to_string()),
            "a single-chord override reports its chord: {notes:?}"
        );
    }
}
