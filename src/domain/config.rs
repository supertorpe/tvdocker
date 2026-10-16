//! Config : schema ported from tvdocker.toml, renamed.
//! `keymap()` and its input-module dependency were NOT ported — the
//! `[keybindings]` overrides are tvdocker's own (settings).

use std::collections::{BTreeMap, HashMap};
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

/// One column's persisted layout — the shape of the upstream `ColumnState`
/// (see [`ColumnLayoutCfg::to_states`] / [`ColumnLayoutCfg::from_states`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ColumnLayoutEntry {
    pub column: usize,
    pub order: usize,
    pub width: i32,
    pub flex: u16,
    pub fixed: bool,
    pub hidden: bool,
}

/// One table's persisted column layout: read back at window build
/// ([`column_layout`](TvdockerConfig::column_layout)) and rewritten on
/// `TABLE_COLUMNS_CHANGED`, so order / width / flex / fixed / hidden survive a
/// restart. (Replaces the vestigial per-table `*Columns` show/hide structs.)
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ColumnLayoutCfg {
    pub layout: Vec<ColumnLayoutEntry>,
}

impl ColumnLayoutCfg {
    /// Snapshot a table's layout (upstream `Table::column_layout`) for saving.
    pub fn from_states(states: &[tv::ColumnState]) -> Self {
        Self {
            layout: states
                .iter()
                .map(|s| ColumnLayoutEntry {
                    column: s.column,
                    order: s.order,
                    width: s.width,
                    flex: s.flex,
                    fixed: s.fixed,
                    hidden: s.hidden,
                })
                .collect(),
        }
    }

    /// The states to hand to `Table::set_column_layout` at window build.
    pub fn to_states(&self) -> Vec<tv::ColumnState> {
        self.layout
            .iter()
            .map(|e| tv::ColumnState {
                column: e.column,
                order: e.order,
                width: e.width,
                flex: e.flex,
                fixed: e.fixed,
                hidden: e.hidden,
            })
            .collect()
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
    /// Per-table column layout, keyed by a stable table id ("containers",
    /// "images", "explorer.host", …). Absent ⇒ the built-in column defaults.
    #[serde(default)]
    pub column_layouts: BTreeMap<String, ColumnLayoutCfg>,
    #[serde(default = "default_true")]
    pub mouse: bool,
    #[serde(default)]
    pub llm: Option<LlmConfig>,
    /// The selected theme's name; `None` = the built-in `classic`.
    #[serde(default)]
    pub theme: Option<String>,
    /// User-defined themes, keyed by name. A name that matches a preset
    /// (`classic`/`dark`/`light`/`terminal`) replaces that preset.
    #[serde(default)]
    pub themes: HashMap<String, crate::domain::theme::ThemeDef>,
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
            column_layouts: Default::default(),
            mouse: true,
            llm: None,
            theme: None,
            themes: Default::default(),
        }
    }
}

impl TvdockerConfig {
    /// `(field, problem)` for every bad theme colour/role and for a `theme =`
    /// selection that names nothing — the keybindings reporting contract, so
    /// the Settings window lists them beside the binding problems.
    pub fn validate_themes(&self) -> Vec<(String, String)> {
        crate::domain::theme::validate(&self.themes, &self.theme)
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

    /// Persist ONLY the top-level `theme = "…"` line, leaving the rest of the
    /// file byte-identical. The Settings window edits this file as raw text
    /// (comments and hand-formatting are the user's), so a full re-serialize
    /// here would silently destroy them — [`save`](Self::save) is the
    /// programmatic path for *structured* saves, this one is a line edit.
    ///
    /// Call [`rewrite_theme_line`] for the text transformation; this wrapper
    /// only reads, creates the directory, and writes.
    pub fn set_theme_in_file(path: &Path, name: &str) -> Result<(), String> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let out = rewrite_theme_line(&text, name);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, out).map_err(|e| e.to_string())
    }

    /// Read one table's persisted column layout, if any.
    pub fn column_layout(&self, key: &str) -> Option<&ColumnLayoutCfg> {
        self.column_layouts.get(key)
    }

    /// Rewrite ONLY the `[column_layouts.<key>]` block in `path`, byte-
    /// preserving everything else — the same line-edit contract as
    /// [`set_theme_in_file`](Self::set_theme_in_file) (comments and
    /// hand-formatting are the user's). `Err(String)` is shown to the user
    /// verbatim.
    pub fn set_column_layout_in_file(
        path: &Path,
        key: &str,
        cfg: &ColumnLayoutCfg,
    ) -> Result<(), String> {
        if !valid_layout_key(key) {
            return Err(format!("{key:?} is not a usable table key"));
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let out = replace_column_layout_block(&text, key, cfg);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, out).map_err(|e| e.to_string())
    }
}

/// Is this line a top-level `theme = …` assignment? (`themes`, `theme_extra`
/// and commented-out lines are NOT.)
fn is_theme_assignment(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("theme") else {
        return false;
    };
    if rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    rest.trim_start().starts_with('=')
}

/// The pure half of [`TvdockerConfig::set_theme_in_file`]:
///
/// * only the ROOT section (before the first `[table]` header) is searched,
///   so `theme` can never be written *inside* `[containers]`;
/// * an existing line keeps its indentation and its trailing comment, and
///   only the value is replaced;
/// * a missing line is inserted after the last non-blank root line (or at the
///   very top when the file opens with a table);
/// * everything else — blank lines, tables, comments — is untouched, and the
///   presence/absence of a final newline is preserved.
pub fn rewrite_theme_line(text: &str, name: &str) -> String {
    let had_trailing_nl = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if had_trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let root_end = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let quoted = format!("theme = \"{name}\"");
    if let Some(i) = lines[..root_end]
        .iter()
        .position(|l| is_theme_assignment(l))
    {
        let old = lines[i].clone();
        let indent: String = old.chars().take_while(|c| c.is_whitespace()).collect();
        // Keep whatever follows the value's closing quote (a trailing note).
        let suffix = match old.find('=').and_then(|eq| {
            let after = &old[eq + 1..];
            after
                .find('"')
                .and_then(|o| after[o + 1..].find('"').map(|c| eq + 1 + o + 1 + c + 1))
        }) {
            Some(pos) => &old[pos..],
            None => "",
        };
        lines[i] = format!("{indent}{quoted}{suffix}");
    } else {
        // Group with the other ROOT KEYS: after the last key assignment (so a
        // trailing comment block that documents the next table keeps its
        // table). A line that OPENS a construct (`foo = [` / `= {`) is not a
        // safe neighbour — inserting under it would land inside the array —
        // so those are skipped, as are comments. Fall back to the last
        // non-blank line, then the top of the file.
        let keys = lines[..root_end].iter().rposition(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && l.contains('=') && !t.ends_with(['[', ',', ':'])
        });
        let at = keys.map(|i| i + 1).unwrap_or_else(|| {
            lines[..root_end]
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map(|i| i + 1)
                .unwrap_or(0)
        });
        lines.insert(at, quoted);
    }
    let mut out = lines.join("\n");
    if had_trailing_nl {
        out.push('\n');
    }
    out
}

/// A TOML bare key when the name allows it, otherwise a quoted key.
fn toml_key(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        name.to_string()
    } else {
        toml_string(name)
    }
}

/// A TOML basic string (our data is ASCII — escapes quotes and backslashes).
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Is `name` safe to write both as a table key and as `theme = "…"`? Empty,
/// quoted, or containing a control character would corrupt the selection
/// line, so the caller validates before writing.
pub fn valid_theme_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 64
        && !trimmed.contains('"')
        && !trimmed.chars().any(char::is_control)
}

/// The TOML block defining `name`: the slot table (only when the def carries
/// slots) plus the role-override table, roles sorted so output is stable.
/// `[themes.x.roles]` alone is enough when there are no slots — TOML creates
/// the parent table implicitly.
pub fn theme_table_text(name: &str, def: &crate::domain::theme::ThemeDef) -> String {
    let key = toml_key(name);
    let slots: [(&str, &Option<String>); 10] = [
        ("background", &def.background),
        ("foreground", &def.foreground),
        ("border", &def.border),
        ("accent", &def.accent),
        ("surface", &def.surface),
        ("muted", &def.muted),
        ("error", &def.error),
        ("warning", &def.warning),
        ("info", &def.info),
        ("success", &def.success),
    ];
    let mut out = String::new();
    if slots.iter().any(|(_, v)| v.is_some()) {
        out.push_str(&format!("[themes.{key}]\n"));
        for (slot, value) in slots {
            if let Some(value) = value {
                out.push_str(&format!("{slot} = {}\n", toml_string(value)));
            }
        }
    }
    if !def.roles.is_empty() {
        out.push_str(&format!("[themes.{key}.roles]\n"));
        let mut names: Vec<&String> = def.roles.keys().collect();
        names.sort();
        for name in names {
            out.push_str(&format!(
                "{} = {}\n",
                toml_key(name),
                toml_string(&def.roles[name])
            ));
        }
    }
    out
}

/// Replace `name`'s block in `text` (or append it), leaving every other byte
/// alone. The block spans `[themes.<key>]` AND its `[themes.<key>.roles]`
/// sub-table — a header's trailing comment or spaces are tolerated.
pub fn replace_theme_table(text: &str, name: &str, def: &crate::domain::theme::ThemeDef) -> String {
    let key = toml_key(name);
    let parent = format!("[themes.{key}]");
    let roles = format!("[themes.{key}.roles]");
    let had_trailing_nl = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if had_trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut skipping = false;
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    let mut insert_at = None;
    for line in lines {
        let t = line.trim_start();
        if t.starts_with('[') {
            let head = t.find(']').map(|i| &t[..=i]).unwrap_or("");
            let ours = head == parent || head == roles;
            if ours && insert_at.is_none() {
                insert_at.get_or_insert(kept.len());
            }
            skipping = ours;
        }
        if !skipping {
            kept.push(line);
        }
    }
    // Replace IN PLACE (a comment above the block keeps documenting it);
    // append at the end when the theme is new — with a blank line so the new
    // table doesn't weld itself onto the last root key.
    let at = match insert_at {
        Some(at) => at,
        None => {
            if !kept.is_empty() && !kept.last().is_some_and(|l| l.trim().is_empty()) {
                kept.push(String::new());
            }
            kept.len()
        }
    };
    let block: Vec<String> = theme_table_text(name, def)
        .lines()
        .map(str::to_string)
        .collect();
    let mut out_lines = kept;
    for (i, line) in block.into_iter().enumerate() {
        out_lines.insert(at + i, line);
    }
    let mut out = out_lines.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if had_trailing_nl && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Is `key` safe as a `[column_layouts.<key>]` table key? Bare keys (ASCII
/// letters, digits, `_`, `-`, `.`) keep the file readable. `.` is allowed so
/// ids like `explorer.host` work; the writer quotes such a key TOML-style.
pub fn valid_layout_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The TOML block for one table's column layout: the `[column_layouts.<key>]`
/// header plus a `layout` array, one inline table per column (stored order).
pub fn column_layout_table_text(key: &str, cfg: &ColumnLayoutCfg) -> String {
    let k = toml_key(key);
    let mut out = format!("[column_layouts.{k}]\n");
    if cfg.layout.is_empty() {
        out.push_str("layout = []\n");
        return out;
    }
    out.push_str("layout = [\n");
    for e in &cfg.layout {
        out.push_str(&format!(
            "  {{ column = {}, order = {}, width = {}, flex = {}, fixed = {}, hidden = {} }},\n",
            e.column, e.order, e.width, e.flex, e.fixed, e.hidden
        ));
    }
    out.push_str("]\n");
    out
}

/// Replace `key`'s `[column_layouts.<key>]` block in `text` (or append it),
/// leaving every other byte alone. Modeled on [`replace_theme_table`], so it
/// shares that helper's behaviour: the block *and one trailing blank line* are
/// replaced, and everything else — comments, ordering, other tables — is
/// untouched.
pub fn replace_column_layout_block(text: &str, key: &str, cfg: &ColumnLayoutCfg) -> String {
    let parent = format!("[column_layouts.{}]", toml_key(key));
    let had_trailing_nl = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if had_trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut skipping = false;
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    let mut insert_at = None;
    for line in lines {
        let t = line.trim_start();
        if t.starts_with('[') {
            let head = t.find(']').map(|i| &t[..=i]).unwrap_or("");
            let ours = head == parent;
            if ours && insert_at.is_none() {
                insert_at.get_or_insert(kept.len());
            }
            skipping = ours;
        }
        if !skipping {
            kept.push(line);
        }
    }
    let at = match insert_at {
        Some(at) => at,
        None => {
            if !kept.is_empty() && !kept.last().is_some_and(|l| l.trim().is_empty()) {
                kept.push(String::new());
            }
            kept.len()
        }
    };
    let block: Vec<String> = column_layout_table_text(key, cfg)
        .lines()
        .map(str::to_string)
        .collect();
    let mut out_lines = kept;
    for (i, line) in block.into_iter().enumerate() {
        out_lines.insert(at + i, line);
    }
    let mut out = out_lines.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if had_trailing_nl && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Persist an authored theme in one pass: write (or replace) its
/// `[themes.<name>]` block AND point the selection at it. Same byte-
/// preservation contract as [`TvdockerConfig::set_theme_in_file`].
pub fn save_theme_as(
    path: &Path,
    name: &str,
    def: &crate::domain::theme::ThemeDef,
) -> Result<(), String> {
    if !valid_theme_name(name) {
        return Err(format!("{name:?} is not a usable theme name"));
    }
    let name = name.trim();
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let out = rewrite_theme_line(&replace_theme_table(&text, name, def), name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, out).map_err(|e| e.to_string())
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
        assert!(
            c.column_layouts.is_empty(),
            "no persisted column layout by default"
        );
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

    #[test]
    fn themes_and_selection_deserialize_from_toml() {
        let path = tmpfile(
            "theme = \"dracula\"\n\
             [themes.dracula]\n\
             background = \"#282a36\"\n\
             foreground = \"#f8f8f2\"\n\
             [themes.dracula.roles]\n\
             ListNormal = \"#f8f8f2/#282a36\"\n",
        );
        let (cfg, err) = TvdockerConfig::load_from(&path);
        assert_eq!(err, None, "clean file");
        assert_eq!(cfg.theme.as_deref(), Some("dracula"));
        let def = cfg.themes.get("dracula").expect("theme def parsed");
        assert_eq!(def.background.as_deref(), Some("#282a36"));
        assert_eq!(
            def.roles.get("ListNormal").map(String::as_str),
            Some("#f8f8f2/#282a36")
        );
        assert!(cfg.validate_themes().is_empty(), "a sane file is clean");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn defaults_have_no_selection_and_no_problems() {
        let cfg = TvdockerConfig::default();
        assert_eq!(cfg.theme, None, "unset ⇒ classic");
        assert!(cfg.themes.is_empty());
        assert!(cfg.validate_themes().is_empty());
    }

    #[test]
    fn validate_themes_lists_field_level_problems() {
        let mut cfg = TvdockerConfig {
            theme: Some("ghost".into()),
            ..Default::default()
        };
        let def = crate::domain::theme::ThemeDef {
            background: Some("bogus".into()),
            ..Default::default()
        };
        cfg.themes.insert("mine".into(), def);
        let problems = cfg.validate_themes();
        let fields: Vec<&str> = problems.iter().map(|(f, _)| f.as_str()).collect();
        assert!(fields.contains(&"themes.mine.background"), "{fields:?}");
        assert!(fields.contains(&"theme"), "dangling selection: {fields:?}");
    }

    #[test]
    fn rewrite_replaces_the_value_and_keeps_comment_indent_and_the_rest() {
        let text =
            "# my config\n  theme = \"dark\"   # the dark one\nfoo = 1\n\n[mouse]\nx = true\n";
        let out = rewrite_theme_line(text, "light");
        assert_eq!(
            out,
            "# my config\n  theme = \"light\"   # the dark one\nfoo = 1\n\n[mouse]\nx = true\n",
            "only the value changes — byte-identical elsewhere"
        );
    }

    #[test]
    fn rewrite_inserts_at_the_end_of_the_root_section() {
        let text = "check_updates = true\n\n# a note\n\n[containers]\nbash = \"zsh\"\n";
        let out = rewrite_theme_line(text, "dark");
        assert_eq!(
            out,
            "check_updates = true\ntheme = \"dark\"\n\n# a note\n\n[containers]\nbash = \"zsh\"\n",
            "after the last non-blank root line, before the table"
        );
    }

    #[test]
    fn rewrite_ignores_lookalikes_and_keeps_a_file_without_a_final_newline() {
        // `themes`, commented lines and a table-scoped lookalike must NOT be
        // mistaken for the selection; a file that ends without a newline stays
        // that way.
        let text = "# theme = \"old\"\nthemes = 1\n\n[containers]\ntheme = \"inner\"";
        let out = rewrite_theme_line(text, "dark");
        assert_eq!(
            out,
            "# theme = \"old\"\nthemes = 1\ntheme = \"dark\"\n\n[containers]\ntheme = \"inner\"",
            "only a real root assignment counts; no trailing newline invented"
        );
        assert!(!out.ends_with('\n'), "trailing newline preserved as absent");
    }

    #[test]
    fn rewrite_handles_a_file_that_opens_with_a_table_and_an_empty_file() {
        assert_eq!(
            rewrite_theme_line("[containers]\nbash = \"zsh\"\n", "dark"),
            "theme = \"dark\"\n[containers]\nbash = \"zsh\"\n"
        );
        assert_eq!(rewrite_theme_line("", "dark"), "theme = \"dark\"\n");
        assert_eq!(
            rewrite_theme_line("theme = \"a\"", "b"),
            "theme = \"b\"",
            "single line, no final newline"
        );
    }

    #[test]
    fn rewrite_never_lands_inside_a_multi_line_value() {
        // `extras = [` opens an array: a neighbour right under it would be a
        // syntax error, so the writer walks past it to the previous key.
        let text = "mouse = true\nextras = [\n  \"a\",\n]\n\n[containers]\nbash = \"zsh\"\n";
        assert_eq!(
            rewrite_theme_line(text, "dark"),
            "mouse = true\ntheme = \"dark\"\nextras = [\n  \"a\",\n]\n\n[containers]\nbash = \"zsh\"\n"
        );
    }

    #[test]
    fn set_theme_in_file_writes_only_the_selection_line() {
        let path = tmpfile("mouse = true\n\n[containers]\nbash = \"zsh\"\n");
        TvdockerConfig::set_theme_in_file(&path, "dark").expect("write");
        let back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            back,
            "mouse = true\ntheme = \"dark\"\n\n[containers]\nbash = \"zsh\"\n"
        );
        // Second write replaces, never duplicates.
        TvdockerConfig::set_theme_in_file(&path, "light").expect("write");
        let back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            back,
            "mouse = true\ntheme = \"light\"\n\n[containers]\nbash = \"zsh\"\n"
        );
        let _ = std::fs::remove_file(path);
    }

    fn def(slots: bool, roles: &[(&str, &str)]) -> crate::domain::theme::ThemeDef {
        let mut d = crate::domain::theme::ThemeDef::default();
        if slots {
            d.background = Some("#101010".into());
            d.foreground = Some("#eeeeee".into());
        }
        for (k, v) in roles {
            d.roles.insert((*k).to_string(), (*v).to_string());
        }
        d
    }

    #[test]
    fn theme_table_text_emits_slots_then_sorted_roles() {
        let text = theme_table_text(
            "mine",
            &def(
                true,
                &[("ListNormal", "#fff/#000"), ("FrameActive", "#a/#b")],
            ),
        );
        assert_eq!(
            text,
            "[themes.mine]\nbackground = \"#101010\"\nforeground = \"#eeeeee\"\n\
             [themes.mine.roles]\nFrameActive = \"#a/#b\"\nListNormal = \"#fff/#000\"\n",
            "slots first, roles sorted: {text:?}"
        );
        // A roles-only def needs no parent header (TOML implies it).
        let roles_only = theme_table_text("solo", &def(false, &[("Shadow", "#000/#111")]));
        assert_eq!(roles_only, "[themes.solo.roles]\nShadow = \"#000/#111\"\n");
        // A name that is not a bare key gets quoted.
        assert!(
            theme_table_text("my theme!", &def(false, &[("Shadow", "#0/#1")]))
                .starts_with("[themes.\"my theme!\".roles]"),
            "quoted key"
        );
    }

    #[test]
    fn a_theme_block_parses_back_as_a_theme_def() {
        let block = theme_table_text("parsed", &def(true, &[("ListNormal", "#fff/#000")]));
        let doc = format!("mouse = true\n{block}");
        let cfg: TvdockerConfig = toml::from_str(&doc).expect("the block is valid TOML");
        let d = cfg.themes.get("parsed").expect("theme parsed");
        assert_eq!(d.background.as_deref(), Some("#101010"));
        assert_eq!(
            d.roles.get("ListNormal").map(String::as_str),
            Some("#fff/#000")
        );
        assert!(cfg.validate_themes().is_empty(), "clean: {block:?}");
    }

    #[test]
    fn replace_theme_table_touches_only_the_named_block() {
        let text = "# fave\nmouse = true\n\n[themes.a]\nbackground = \"#000000\"\n\n\
                    [themes.a.roles]\nShadow = \"#1/#2\"\n\n[themes.b]\nbackground = \"#ffffff\"\n";
        let out = replace_theme_table(text, "a", &def(false, &[("ListNormal", "#fff/#000")]));
        assert_eq!(
            out,
            "# fave\nmouse = true\n\n[themes.a.roles]\nListNormal = \"#fff/#000\"\n\
             [themes.b]\nbackground = \"#ffffff\"\n",
            "a replaced in place, b and the comment byte-identical: {out:?}"
        );
        assert!(!out.contains("#000000"), "the old slot is gone");
    }

    #[test]
    fn save_theme_as_appends_block_and_selection_and_is_idempotent() {
        let path = tmpfile("# hi\nmouse = true\n");
        let d = def(false, &[("ListNormal", "#fff/#000")]);
        save_theme_as(&path, "authored", &d).expect("write");
        let first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            first,
            "# hi\nmouse = true\ntheme = \"authored\"\n\n[themes.authored.roles]\n\
             ListNormal = \"#fff/#000\"\n",
            "selection in the root section, block appended after a blank line: {first:?}"
        );
        // Second save replaces, never duplicates.
        save_theme_as(
            &path,
            "authored",
            &def(false, &[("Shadow", "#333333/#444444")]),
        )
        .expect("rewrite");
        let second = std::fs::read_to_string(&path).unwrap();
        assert!(!second.contains("ListNormal"), "old roles gone: {second:?}");
        assert!(
            second.contains("Shadow = \"#333333/#444444\""),
            "new roles: {second:?}"
        );
        assert_eq!(
            second.matches("theme = ").count(),
            1,
            "one selection line: {second:?}"
        );
        // And the file still parses clean.
        let (cfg, err) = TvdockerConfig::load_from(&path);
        assert_eq!(err, None, "round trip parses");
        assert_eq!(cfg.theme.as_deref(), Some("authored"));
        assert!(cfg.validate_themes().is_empty());
        // A bad name never writes.
        assert!(save_theme_as(&path, "bad\"name", &d).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn valid_theme_names_are_short_and_quotable() {
        assert!(valid_theme_name("midnight"));
        assert!(valid_theme_name("My Theme 2"));
        assert!(!valid_theme_name(""));
        assert!(!valid_theme_name("   "));
        assert!(!valid_theme_name("he said \"hi\""));
        assert!(!valid_theme_name(&"x".repeat(65)));
        assert!(!valid_theme_name("line\nbreak"));
    }

    fn layout_entry(column: usize, order: usize, width: i32) -> ColumnLayoutEntry {
        ColumnLayoutEntry {
            column,
            order,
            width,
            flex: 0,
            fixed: false,
            hidden: false,
        }
    }

    #[test]
    fn column_layout_block_replaces_only_itself() {
        let text = "# mine\nmouse = true\n\n[column_layouts.containers]\n\
                    layout = [\n  { column = 9, order = 9, width = 1, flex = 7, fixed = true, hidden = true },\n]\n\n\
                    [themes.x]\nbackground = \"#000000\"\n";
        let cfg = ColumnLayoutCfg {
            layout: vec![layout_entry(0, 1, 20), layout_entry(1, 0, 12)],
        };
        let out = replace_column_layout_block(text, "containers", &cfg);
        let expected = "# mine\nmouse = true\n\n[column_layouts.containers]\nlayout = [\n  { column = 0, order = 1, width = 20, flex = 0, fixed = false, hidden = false },\n  { column = 1, order = 0, width = 12, flex = 0, fixed = false, hidden = false },\n]\n[themes.x]\nbackground = \"#000000\"\n";
        assert_eq!(out, expected, "only the block changes, themes untouched");
    }

    #[test]
    fn column_layout_block_appends_when_absent() {
        let text = "# hi\nmouse = true\n";
        let cfg = ColumnLayoutCfg {
            layout: vec![layout_entry(3, 0, 7)],
        };
        let out = replace_column_layout_block(text, "images", &cfg);
        let expected = "# hi\nmouse = true\n\n[column_layouts.images]\nlayout = [\n  { column = 3, order = 0, width = 7, flex = 0, fixed = false, hidden = false },\n]\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn column_layout_of_no_entries_is_an_empty_array() {
        let out =
            replace_column_layout_block("mouse = true\n", "volumes", &ColumnLayoutCfg::default());
        assert_eq!(
            out,
            "mouse = true\n\n[column_layouts.volumes]\nlayout = []\n"
        );
    }

    #[test]
    fn a_hand_written_column_layout_parses_and_the_rest_survives() {
        let path = tmpfile(
            "theme = \"dark\"\n\n[column_layouts.containers]\nlayout = [\n  { column = 0, order = 1, width = 16, flex = 2, fixed = false, hidden = true },\n]\n",
        );
        let (c, err) = TvdockerConfig::load_from(&path);
        assert_eq!(err, None);
        let cfg = c.column_layout("containers").expect("parsed");
        assert_eq!(
            cfg.layout,
            vec![ColumnLayoutEntry {
                column: 0,
                order: 1,
                width: 16,
                flex: 2,
                fixed: false,
                hidden: true,
            }]
        );
        assert_eq!(c.theme.as_deref(), Some("dark"), "the rest still parses");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn set_column_layout_in_file_is_byte_preserving_and_reloadable() {
        let path = tmpfile("mouse = true\n\n[themes.x]\nbackground = \"#000000\"\n");
        let cfg = ColumnLayoutCfg {
            layout: vec![layout_entry(0, 0, 20)],
        };
        TvdockerConfig::set_column_layout_in_file(&path, "containers", &cfg).expect("write");
        let back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            back,
            "mouse = true\n\n[themes.x]\nbackground = \"#000000\"\n\n[column_layouts.containers]\nlayout = [\n  { column = 0, order = 0, width = 20, flex = 0, fixed = false, hidden = false },\n]\n"
        );
        // It parses back, and a dotted key is quoted (explorer.host).
        let (c, err) = TvdockerConfig::load_from(&path);
        assert_eq!(err, None);
        assert_eq!(c.column_layout("containers"), Some(&cfg));
        TvdockerConfig::set_column_layout_in_file(&path, "explorer.host", &cfg).expect("write");
        let back = std::fs::read_to_string(&path).unwrap();
        assert!(
            back.contains("[column_layouts.\"explorer.host\"]"),
            "dotted key quoted: {back:?}"
        );
        assert_eq!(
            TvdockerConfig::load_from(&path)
                .0
                .column_layout("explorer.host"),
            Some(&cfg)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn set_column_layout_in_file_rejects_unusable_keys() {
        let path = tmpfile("mouse = true\n");
        let cfg = ColumnLayoutCfg::default();
        for bad in ["", "bad key", "bad\"key", "bad\nkey"] {
            assert!(
                TvdockerConfig::set_column_layout_in_file(&path, bad, &cfg).is_err(),
                "{bad:?} rejected"
            );
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "mouse = true\n",
            "nothing written"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_malformed_column_layout_block_is_not_fatal() {
        let path = tmpfile("[column_layouts.containers]\nlayout = [ { column = } ]\n");
        let (c, err) = TvdockerConfig::load_from(&path);
        assert!(err.is_some(), "the parse error is surfaced");
        assert_eq!(c.column_layout("containers"), None, "defaults, no panic");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn column_layout_maps_to_and_from_upstream_states() {
        let states = vec![
            tv::ColumnState {
                column: 2,
                order: 0,
                width: 8,
                flex: 1,
                fixed: true,
                hidden: false,
            },
            tv::ColumnState {
                column: 0,
                order: 1,
                width: 20,
                flex: 0,
                fixed: false,
                hidden: true,
            },
        ];
        let cfg = ColumnLayoutCfg::from_states(&states);
        assert_eq!(cfg.layout[0].column, 2);
        assert!(cfg.layout[1].hidden);
        assert_eq!(cfg.to_states(), states, "round trip");
    }
}
