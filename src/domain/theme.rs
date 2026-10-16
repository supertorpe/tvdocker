//! Themes : named color presets in `tvdocker.toml`.
//!
//! A theme is a handful of NAMED SLOTS (`background`, `foreground`, `border`,
//! `accent`, `surface`, `muted` + the four severity colors) that expand into
//! tvision's 80 [`Role`]s by the family rules below, plus an optional
//! per-role escape hatch (`[themes.<name>.roles]`, keys are [`Role::name`]).
//!
//! Presets: [`PRESETS`]. `classic` is SPECIAL — it is tvision's stock
//! `classic_blue()` verbatim (no expansion), so an unset `theme =` key keeps
//! the exact look the app has always shipped. Every other theme goes through
//! [`expand`], which starts from `classic_blue()` and overwrites only
//! `fg`/`bg` — never `modifiers`, so underline/shortcut styles survive.

use std::collections::HashMap;
use tv::{Color, Role, Theme};

/// The named slots a theme may set. All optional: anything omitted is
/// derived (see [`Slots::resolve`]).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThemeDef {
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub foreground: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub muted: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub info: Option<String>,
    #[serde(default)]
    pub success: Option<String>,
    /// Per-role overrides: `Role::name()` → `"fg/bg"` (both halves required).
    #[serde(default)]
    pub roles: HashMap<String, String>,
}

/// Built-in theme names, in picker order. A `[themes.<name>]` entry with one
/// of these names REPLACES the preset.
pub const PRESETS: [&str; 4] = ["classic", "dark", "light", "terminal"];

/// `#[serde(default)]` helper: a theme with no `[themes.x]` body at all.
pub fn default_theme_def() -> ThemeDef {
    ThemeDef::default()
}

// ---------------------------------------------------------------------------
// Colour strings
// ---------------------------------------------------------------------------

/// Parse one colour: `#rgb`, `#rrggbb`, a 16-colour name, `idx(0..255)`, or
/// `default` (the terminal's own colour). Names are case-insensitive and
/// accept `-`/space separators; `yellow` is the BRIGHT yellow (14) — use
/// `brown` for the dim one (6).
pub fn parse_color(spec: &str) -> Result<Color, String> {
    let s = spec.trim();
    let bad = || format!("{s:?} — want #hex, a colour name, idx(0..255) or default");
    if s.eq_ignore_ascii_case("default") {
        return Ok(Color::Default);
    }
    if let Some(hex) = s.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().filter_map(|c| c.to_digit(16)).collect();
        if digits.len() != hex.len() || !(hex.len() == 3 || hex.len() == 6) {
            return Err(bad());
        }
        let byte = |hi: u32, lo: u32| ((hi << 4) | lo) as u8;
        return Ok(if hex.len() == 3 {
            Color::Rgb(
                byte(digits[0], digits[0]),
                byte(digits[1], digits[1]),
                byte(digits[2], digits[2]),
            )
        } else {
            Color::Rgb(
                byte(digits[0], digits[1]),
                byte(digits[2], digits[3]),
                byte(digits[4], digits[5]),
            )
        });
    }
    if let Some(inner) = s.strip_prefix("idx(").and_then(|r| r.strip_suffix(')')) {
        let n: u8 = inner.trim().parse().map_err(|_| bad())?;
        return Ok(Color::Indexed(n));
    }
    let key: String = s
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | '_'))
        .collect();
    let bios = match key.as_str() {
        "black" => 0,
        "blue" => 1,
        "green" => 2,
        "cyan" => 3,
        "red" => 4,
        "magenta" | "purple" => 5,
        "brown" | "orange" => 6,
        "lightgray" | "gray" | "grey" | "silver" => 7,
        "darkgray" | "darkgrey" => 8,
        "lightblue" => 9,
        "lightgreen" => 10,
        "lightcyan" => 11,
        "lightred" | "pink" => 12,
        "lightmagenta" | "violet" => 13,
        "yellow" | "lightyellow" => 14,
        "white" | "lightwhite" => 15,
        _ => return Err(bad()),
    };
    Ok(Color::Bios(bios))
}

/// `#rrggbb` (or `default`) for display in the picker's detail line.
pub fn color_hex(c: Color) -> String {
    match c {
        Color::Default => "default".to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        other => match rgb(other) {
            Some((r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
            None => "default".to_string(),
        },
    }
}

/// Resolve a colour to RGB, for mixing/luminance. `Color::Default` has no
/// RGB — it is the terminal's own colour, deliberately unknown.
fn rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Bios(n) => Some(Color::BIOS_RGB[usize::from(n & 0x0f)]),
        Color::Indexed(n) => Some(xterm256(n)),
        Color::Default => None,
    }
}

/// The standard xterm-256 palette in RGB.
fn xterm256(n: u8) -> (u8, u8, u8) {
    match n {
        0..=15 => Color::BIOS_RGB[usize::from(n)],
        16..=231 => {
            let i = usize::from(n - 16);
            let step = |v: usize| if v == 0 { 0 } else { 55 + 40 * v as u8 };
            (step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        _ => {
            let v = if n == 255 { 255 } else { 8 + 10 * (n - 232) };
            (v, v, v)
        }
    }
}

/// Mix two colours (`t` = weight of `a`). An unmixable input (the terminal's
/// `default`) makes the result fall back to `b`.
fn mix(a: Color, b: Color, t: f32) -> Color {
    match (rgb(a), rgb(b)) {
        (Some((ar, ag, ab)), Some((br, bg, bb))) => {
            let l = |x: u8, y: u8| (f32::from(x) * t + f32::from(y) * (1.0 - t)).round() as u8;
            Color::Rgb(l(ar, br), l(ag, bg), l(ab, bb))
        }
        _ => b,
    }
}

/// Perceived luminance 0..=1 (Rec. 709); `default` is treated as mid grey.
fn luminance(c: Color) -> f32 {
    match rgb(c) {
        Some((r, g, b)) => (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0,
        None => 0.5,
    }
}

/// Black or white — whichever reads better on `c`.
fn contrast(c: Color) -> Color {
    if luminance(c) > 0.55 {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    }
}

// ---------------------------------------------------------------------------
// Slots → Theme
// ---------------------------------------------------------------------------

/// Fully resolved slot values (after defaults + derivation).
#[derive(Clone, Copy, Debug)]
struct Slots {
    bg: Color,
    fg: Color,
    border: Color,
    accent: Color,
    surface: Color,
    muted: Color,
    error: Color,
    warning: Color,
    info: Color,
    success: Color,
}

impl Slots {
    /// Fill a partial [`ThemeDef`]. Defaults are derived so a two-slot theme
    /// still renders coherently:
    ///
    /// * `background`/`foreground` fall back to the classic theme's
    ///   (lightgray-on-blue), so omitting them never explodes;
    /// * `muted = mix(fg, bg, 50%)`, `border = muted`, `accent = fg`,
    ///   `surface = mix(bg, fg, 10%)` (a slightly lifted panel — dialogs,
    ///   lists and the status bar sit on it);
    /// * severity colours fall back to ANSI red/yellow/cyan/green, darkened
    ///   on a light background so they stay readable.
    fn resolve(def: &ThemeDef) -> Self {
        let classic = Theme::classic_blue();
        let pick = |spec: &Option<String>, fallback: Color| {
            spec.as_deref()
                .and_then(|s| parse_color(s).ok())
                .unwrap_or(fallback)
        };
        let bg = pick(&def.background, classic.style(Role::Background).bg);
        let fg = pick(&def.foreground, classic.style(Role::Background).fg);
        let muted = pick(&def.muted, mix(fg, bg, 0.5));
        let light = luminance(bg) > 0.55;
        let severity = |spec: &Option<String>, vivid: (u8, u8, u8), deep: (u8, u8, u8)| {
            let (r, g, b) = if light { deep } else { vivid };
            pick(spec, Color::Rgb(r, g, b))
        };
        Self {
            bg,
            fg,
            border: pick(&def.border, muted),
            accent: pick(&def.accent, fg),
            surface: pick(&def.surface, mix(bg, fg, 0.1)),
            muted,
            error: severity(&def.error, (243, 139, 168), (176, 0, 32)),
            warning: severity(&def.warning, (249, 226, 175), (122, 92, 0)),
            info: severity(&def.info, (137, 220, 235), (0, 95, 122)),
            success: severity(&def.success, (166, 227, 161), (10, 107, 46)),
        }
    }
}

/// Overwrite one role's colours, KEEPING its modifiers (classic pins
/// underline/bold styles that a fresh [`Style`] would wipe).
fn paint(theme: &mut Theme, role: Role, fg: Color, bg: Color) {
    let mut s = theme.style(role);
    s.fg = fg;
    s.bg = bg;
    theme.set_style(role, s);
}

/// The family table: slots → all 80 roles. Rules (fg, bg) unless noted:
///
/// * `Background` = muted-on-background (the desktop's `░` texture);
/// * **frames double as the interior fill** (tvision fills the whole body with
///   the frame role), so window frames carry `background` and dialog frames
///   carry `surface`; active = accent, resting = border, dragging/icon = accent;
/// * cyan frames are the "highlighted window" family: accent body;
/// * selected/pressed/focused = `contrast(accent)` ON accent; shortcuts swap
///   only the foreground to accent; disabled/inactive = muted;
/// * severity roles put their colour on `background`;
/// * the status bar fills itself with `StatusNormal`, so it sits on `surface`;
/// * menu boxes are framed like windows, so menu items sit on `background`.
fn apply_slots(theme: &mut Theme, s: &Slots) {
    let Slots {
        bg,
        fg,
        border,
        accent,
        surface,
        muted,
        error,
        warning,
        info,
        success,
    } = *s;
    let on_accent = contrast(accent);

    // Desktop texture.
    paint(theme, Role::Background, muted, bg);

    // Blue-scheme frames = window interiors.
    paint(theme, Role::FrameActive, accent, bg);
    paint(theme, Role::FramePassive, border, bg);
    paint(theme, Role::FrameDragging, accent, bg);
    paint(theme, Role::FrameIcon, accent, bg);
    // Gray-scheme frames = dialog interiors.
    paint(theme, Role::FrameGrayActive, accent, surface);
    paint(theme, Role::FrameGrayPassive, border, surface);
    paint(theme, Role::FrameGrayDragging, accent, surface);
    paint(theme, Role::FrameGrayIcon, accent, surface);
    // Cyan-scheme frames = highlighted windows.
    paint(theme, Role::FrameCyanActive, on_accent, accent);
    paint(theme, Role::FrameCyanPassive, bg, accent);
    paint(theme, Role::FrameCyanDragging, on_accent, accent);
    paint(theme, Role::FrameCyanIcon, on_accent, accent);

    // Scrollbars (arrows in accent on the track).
    paint(theme, Role::ScrollBarPage, muted, surface);
    paint(theme, Role::ScrollBarControls, accent, surface);

    // Generic control states.
    paint(theme, Role::Normal, fg, surface);
    paint(theme, Role::Focused, on_accent, accent);
    paint(theme, Role::Disabled, muted, surface);
    paint(theme, Role::Pressed, on_accent, accent);

    // Lists / trees.
    paint(theme, Role::ListNormal, fg, bg);
    paint(theme, Role::ListInactive, muted, bg);
    paint(theme, Role::ListSurface, fg, surface);
    paint(theme, Role::ListFocused, on_accent, accent);
    paint(theme, Role::ListSelected, accent, surface);
    paint(theme, Role::ListDivider, muted, surface);
    paint(theme, Role::OutlineNormal, fg, bg);
    paint(theme, Role::OutlineInactive, muted, bg);
    paint(theme, Role::OutlineSurface, fg, surface);
    paint(theme, Role::OutlineFocused, on_accent, accent);
    paint(theme, Role::OutlineSelected, accent, surface);
    paint(theme, Role::OutlineNotExpanded, muted, bg);

    // Severity (status line + panes).
    paint(theme, Role::Error, error, bg);
    paint(theme, Role::Warning, warning, bg);
    paint(theme, Role::Info, info, bg);
    paint(theme, Role::Success, success, bg);

    // Dialog text.
    paint(theme, Role::StaticText, fg, surface);
    paint(theme, Role::InfoPane, fg, bg);
    paint(theme, Role::Shadow, muted, bg);

    // Clusters / indicators / labels / buttons (dialog furniture on surface).
    paint(theme, Role::ClusterNormal, fg, surface);
    paint(theme, Role::ClusterSelected, on_accent, accent);
    paint(theme, Role::ClusterNormalShortcut, accent, surface);
    paint(theme, Role::ClusterSelectedShortcut, on_accent, accent);
    paint(theme, Role::ClusterDisabled, muted, surface);
    paint(theme, Role::IndicatorNormal, fg, surface);
    paint(theme, Role::IndicatorDragging, accent, surface);
    paint(theme, Role::LabelNormal, fg, surface);
    paint(theme, Role::LabelLight, muted, surface);
    paint(theme, Role::LabelNormalShortcut, accent, surface);
    paint(theme, Role::LabelLightShortcut, accent, surface);
    paint(theme, Role::ButtonNormal, fg, surface);
    paint(theme, Role::ButtonDefault, on_accent, accent);
    paint(theme, Role::ButtonSelected, on_accent, accent);
    paint(theme, Role::ButtonDisabled, muted, surface);
    paint(theme, Role::ButtonNormalShortcut, accent, surface);
    paint(theme, Role::ButtonDefaultShortcut, on_accent, accent);
    paint(theme, Role::ButtonSelectedShortcut, on_accent, accent);
    paint(theme, Role::ButtonShadow, muted, surface);

    // Inputs: a field is a small sunken area between window and panel colour,
    // so it reads on BOTH a window and a dialog.
    let field = mix(bg, surface, 0.5);
    paint(theme, Role::InputNormal, fg, field);
    paint(theme, Role::InputInactive, muted, field);
    paint(theme, Role::InputSurface, fg, field);
    paint(theme, Role::InputSelected, on_accent, accent);
    paint(theme, Role::InputArrow, accent, field);

    // Scrollers (the pane fill the terminal uses).
    paint(theme, Role::ScrollerNormal, fg, bg);
    paint(theme, Role::ScrollerSelected, fg, surface);

    // Menus (framed like windows ⇒ items sit on background).
    paint(theme, Role::MenuNormal, fg, bg);
    paint(theme, Role::MenuNormalShortcut, accent, bg);
    paint(theme, Role::MenuSelected, on_accent, accent);
    paint(theme, Role::MenuSelectedShortcut, on_accent, accent);
    paint(theme, Role::MenuDisabled, muted, bg);
    paint(theme, Role::MenuSelectedDisabled, muted, accent);

    // Status line (fills itself with StatusNormal ⇒ surface strip).
    paint(theme, Role::StatusNormal, fg, surface);
    paint(theme, Role::StatusShortcut, accent, surface);
    paint(theme, Role::StatusSelect, on_accent, accent);
    paint(theme, Role::StatusShortcutSelect, on_accent, accent);
    paint(theme, Role::StatusDisabled, muted, surface);
    paint(theme, Role::StatusSelDisabled, muted, accent);

    // History window.
    paint(theme, Role::HistoryArrow, on_accent, accent);
    paint(theme, Role::HistorySides, border, surface);
    paint(theme, Role::HistoryViewerNormal, fg, bg);
    paint(theme, Role::HistoryViewerFocused, on_accent, accent);
}

/// `[themes.<name>.roles]` overrides — applied last, so they beat the slots.
fn apply_overrides(theme: &mut Theme, roles: &HashMap<String, String>) {
    for (name, spec) in roles {
        let Some(role) = role_by_name(name) else {
            continue; // validate() reports it; the rest of the theme still applies
        };
        let Some((fg, bg)) = spec.split_once('/') else {
            continue;
        };
        let (Ok(fg), Ok(bg)) = (parse_color(fg), parse_color(bg)) else {
            continue;
        };
        paint(theme, role, fg, bg);
    }
}

/// Build a theme from a def: classic base + slots + per-role overrides.
pub fn expand(def: &ThemeDef) -> Theme {
    let mut theme = Theme::classic_blue();
    apply_slots(&mut theme, &Slots::resolve(def));
    apply_overrides(&mut theme, &def.roles);
    theme
}

/// Built-in definitions for the non-`classic` presets.
fn preset(name: &str) -> Option<ThemeDef> {
    let def = |background: &str,
               foreground: &str,
               border: &str,
               accent: &str,
               surface: &str,
               muted: &str,
               error: &str,
               warning: &str,
               info: &str,
               success: &str| {
        ThemeDef {
            background: Some(background.into()),
            foreground: Some(foreground.into()),
            border: Some(border.into()),
            accent: Some(accent.into()),
            surface: Some(surface.into()),
            muted: Some(muted.into()),
            error: Some(error.into()),
            warning: Some(warning.into()),
            info: Some(info.into()),
            success: Some(success.into()),
            roles: HashMap::new(),
        }
    };
    Some(match name {
        // Catppuccin-mocha-flavoured dark panel.
        "dark" => def(
            "#1e1e2e", "#cdd6f4", "#585b70", "#89b4fa", "#313244", "#6c7086", "#f38ba8", "#f9e2af",
            "#89dceb", "#a6e3a1",
        ),
        // High-key light panel with a sober accent.
        "light" => def(
            "#ececec", "#1c1c1c", "#a0a0a0", "#0b6bcb", "#ffffff", "#767676", "#b00020", "#7a5c00",
            "#005f7a", "#0a6b2e",
        ),
        // Pure passthrough: the terminal's own colours everywhere.
        "terminal" => def(
            "default", "default", "default", "default", "default", "default", "default", "default",
            "default", "default",
        ),
        // `classic` never reaches here (resolve short-circuits it).
        _ => return None,
    })
}

/// Every selectable name: presets in order, then user-only names sorted.
/// A user `[themes.<dark>]` replaces the preset but keeps its slot.
pub fn theme_names(defs: &HashMap<String, ThemeDef>) -> Vec<String> {
    let mut names: Vec<String> = PRESETS.iter().map(|s| (*s).to_string()).collect();
    let mut extra: Vec<String> = defs
        .keys()
        .filter(|k| !PRESETS.contains(&k.as_str()))
        .cloned()
        .collect();
    extra.sort();
    names.append(&mut extra);
    names
}

/// Resolve a name to a theme: a user def wins, then the preset, then
/// `classic` (so an unknown or unset selection never loses the app).
pub fn resolve(defs: &HashMap<String, ThemeDef>, name: &str) -> Theme {
    if let Some(def) = defs.get(name) {
        return expand(def);
    }
    if name == "classic" {
        return Theme::classic_blue();
    }
    match preset(name) {
        Some(def) => expand(&def),
        None => Theme::classic_blue(),
    }
}

/// Serialize a palette back into a def: every role as an explicit
/// `"fg/bg"` override, no slots.
///
/// `expand` starts from `classic_blue()` and applies role overrides LAST, so
/// the colors round-trip exactly — `classic`'s BIOS colours come back as the
/// equivalent RGB (the backend resolves both to the same pixels), which is
/// why the round-trip test compares [`color_hex`] strings rather than `Color`
/// variants. Modifiers ride on the classic base, which is what every editor
/// path starts from (only colors are ever edited).
pub fn theme_to_def(theme: &Theme) -> ThemeDef {
    let mut roles = HashMap::with_capacity(ALL_ROLES.len());
    for role in ALL_ROLES {
        let s = theme.style(role);
        roles.insert(
            role.name().to_string(),
            format!("{}/{}", color_hex(s.fg), color_hex(s.bg)),
        );
    }
    ThemeDef {
        roles,
        ..ThemeDef::default()
    }
}

/// The picker's detail line for `name`: the four headline colours, resolved
/// exactly as the resolver sees them (so a partial theme shows its derived
/// values, not its omissions).
pub fn summary_for(defs: &HashMap<String, ThemeDef>, name: &str) -> String {
    if name == "classic" && !defs.contains_key(name) {
        return "stock Turbo Vision blue".to_string();
    }
    let fallback = preset(name);
    let Some(def) = defs.get(name).or(fallback.as_ref()) else {
        return "unknown theme".to_string();
    };
    let s = Slots::resolve(def);
    format!(
        "bg {} · fg {} · border {} · accent {}",
        color_hex(s.bg),
        color_hex(s.fg),
        color_hex(s.border),
        color_hex(s.accent)
    )
}

/// `(field, problem)` for every bad colour / unknown role / dangling
/// selection — the keybindings contract, so Settings reports them the same way.
pub fn validate(
    defs: &HashMap<String, ThemeDef>,
    selection: &Option<String>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (theme_name, def) in defs {
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
        for (slot, spec) in slots {
            if let Some(spec) = spec
                && parse_color(spec).is_err()
            {
                out.push((
                    format!("themes.{theme_name}.{slot}"),
                    format!("{spec:?} is not a colour"),
                ));
            }
        }
        for (role, spec) in &def.roles {
            if role_by_name(role).is_none() {
                out.push((
                    format!("themes.{theme_name}.roles.{role}"),
                    "unknown role (see the theme editor for role names)".to_string(),
                ));
                continue;
            }
            match spec.split_once('/') {
                None => out.push((
                    format!("themes.{theme_name}.roles.{role}"),
                    format!("{spec:?} must be \"fg/bg\""),
                )),
                Some((fg, bg)) => {
                    for part in [fg, bg] {
                        if let Err(e) = parse_color(part) {
                            out.push((format!("themes.{theme_name}.roles.{role}"), e));
                        }
                    }
                }
            }
        }
    }
    if let Some(sel) = selection
        && !defs.contains_key(sel)
        && !PRESETS.contains(&sel.as_str())
    {
        out.push((
            "theme".to_string(),
            format!("unknown theme {sel:?} — known: {}", PRESETS.join(", ")),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Role name table (tvision keeps `Role::ALL` crate-private; `Role::name` is
// one-way, so we mirror the 80 names here for the override escape hatch).
// ---------------------------------------------------------------------------

/// Reverse of [`Role::name`].
pub fn role_by_name(name: &str) -> Option<Role> {
    ALL_ROLES
        .iter()
        .find(|r| r.name().eq_ignore_ascii_case(name))
        .copied()
}

/// Every [`Role`], index order (mirrors tvision's private `ALL`).
pub const ALL_ROLES: [Role; 80] = [
    Role::Background,
    Role::FrameActive,
    Role::FramePassive,
    Role::FrameDragging,
    Role::FrameIcon,
    Role::ScrollBarPage,
    Role::ScrollBarControls,
    Role::Normal,
    Role::Focused,
    Role::Disabled,
    Role::Pressed,
    Role::ListNormal,
    Role::ListInactive,
    Role::ListSurface,
    Role::ListFocused,
    Role::ListSelected,
    Role::ListDivider,
    Role::Error,
    Role::Warning,
    Role::Info,
    Role::Success,
    Role::StaticText,
    Role::ClusterNormal,
    Role::ClusterSelected,
    Role::ClusterNormalShortcut,
    Role::ClusterSelectedShortcut,
    Role::ClusterDisabled,
    Role::IndicatorNormal,
    Role::IndicatorDragging,
    Role::ButtonNormal,
    Role::ButtonDefault,
    Role::ButtonSelected,
    Role::ButtonDisabled,
    Role::ButtonNormalShortcut,
    Role::ButtonDefaultShortcut,
    Role::ButtonSelectedShortcut,
    Role::ButtonShadow,
    Role::LabelNormal,
    Role::LabelLight,
    Role::LabelNormalShortcut,
    Role::LabelLightShortcut,
    Role::InputNormal,
    Role::InputInactive,
    Role::InputSurface,
    Role::InputSelected,
    Role::InputArrow,
    Role::ScrollerNormal,
    Role::ScrollerSelected,
    Role::MenuNormal,
    Role::MenuNormalShortcut,
    Role::MenuSelected,
    Role::MenuSelectedShortcut,
    Role::MenuDisabled,
    Role::MenuSelectedDisabled,
    Role::StatusNormal,
    Role::StatusShortcut,
    Role::StatusSelect,
    Role::StatusShortcutSelect,
    Role::StatusDisabled,
    Role::StatusSelDisabled,
    Role::InfoPane,
    Role::OutlineNormal,
    Role::OutlineInactive,
    Role::OutlineSurface,
    Role::OutlineFocused,
    Role::OutlineSelected,
    Role::OutlineNotExpanded,
    Role::Shadow,
    Role::FrameGrayActive,
    Role::FrameGrayPassive,
    Role::FrameGrayDragging,
    Role::FrameGrayIcon,
    Role::FrameCyanActive,
    Role::FrameCyanPassive,
    Role::FrameCyanDragging,
    Role::FrameCyanIcon,
    Role::HistoryArrow,
    Role::HistorySides,
    Role::HistoryViewerNormal,
    Role::HistoryViewerFocused,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(pairs: &[(&str, ThemeDef)]) -> HashMap<String, ThemeDef> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn parses_hex_names_index_and_default() {
        assert_eq!(parse_color("#fff"), Ok(Color::Rgb(255, 255, 255)));
        assert_eq!(parse_color("#1e1e2e"), Ok(Color::Rgb(30, 30, 46)));
        assert_eq!(parse_color("Black"), Ok(Color::Bios(0)));
        assert_eq!(parse_color("light-red"), Ok(Color::Bios(12)));
        assert_eq!(parse_color("idx(123)"), Ok(Color::Indexed(123)));
        assert_eq!(parse_color("default"), Ok(Color::Default));
        assert!(parse_color("#12345").is_err(), "odd hex length rejected");
        assert!(parse_color("nope").is_err(), "unknown name rejected");
        assert!(parse_color("idx(999)").is_err(), "range rejected");
    }

    #[test]
    fn classic_is_the_stock_tvision_theme_verbatim() {
        let classic = resolve(&HashMap::new(), "classic");
        assert_eq!(classic, Theme::classic_blue());
        let unset = resolve(&HashMap::new(), "whatever");
        assert_eq!(
            unset,
            Theme::classic_blue(),
            "unknown ⇒ classic, never empty"
        );
    }

    #[test]
    fn dark_preset_expands_to_the_expected_family_rules() {
        let theme = resolve(&HashMap::new(), "dark");
        let dark_bg = Color::Rgb(30, 30, 46);
        let accent = Color::Rgb(0x89, 0xb4, 0xfa);
        let surface = Color::Rgb(0x31, 0x32, 0x44);

        let s = theme.style(Role::Background);
        assert_eq!(s.bg, dark_bg, "desktop sits on the theme background");
        let frame = theme.style(Role::FrameActive);
        assert_eq!(
            (frame.fg, frame.bg),
            (accent, dark_bg),
            "focused frame = accent"
        );
        let dialog = theme.style(Role::FrameGrayActive);
        assert_eq!(
            (dialog.fg, dialog.bg),
            (accent, surface),
            "dialogs sit on surface"
        );
        let focused = theme.style(Role::ListFocused);
        assert_eq!(
            (focused.fg, focused.bg),
            (contrast(accent), accent),
            "the cursor row is the filled accent marker"
        );
        let selected = theme.style(Role::ListSelected);
        assert_eq!(
            (selected.fg, selected.bg),
            (accent, surface),
            "multi-selected rows read as accent text on the panel"
        );
        assert_ne!(
            (focused.fg, focused.bg),
            (selected.fg, selected.bg),
            "cursor and selection stay distinguishable"
        );
        let err = theme.style(Role::Error);
        assert_eq!(err.bg, dark_bg, "severity text sits on the background");
        assert_ne!(err.fg, theme.style(Role::Success).fg, "severities differ");
    }

    #[test]
    fn partial_theme_derives_the_missing_slots() {
        let def = ThemeDef {
            background: Some("#000000".into()),
            foreground: Some("#ffffff".into()),
            ..Default::default()
        };
        let slots = Slots::resolve(&def);
        assert_eq!(slots.accent, slots.fg, "accent defaults to the foreground");
        assert_eq!(slots.border, slots.muted, "border defaults to muted");
        assert_ne!(slots.muted, slots.fg, "muted is a real mix, not the fg");
        assert_eq!(
            slots.error,
            Color::Rgb(243, 139, 168),
            "dark bg ⇒ vivid severity"
        );
        let light = ThemeDef {
            background: Some("#ffffff".into()),
            foreground: Some("#000000".into()),
            ..Default::default()
        };
        let ls = Slots::resolve(&light);
        assert_eq!(ls.error, Color::Rgb(176, 0, 32), "light bg ⇒ deep severity");
    }

    #[test]
    fn role_overrides_beat_the_slots_and_keep_classic_modifiers() {
        let mut def = ThemeDef {
            background: Some("#101010".into()),
            foreground: Some("#eeeeee".into()),
            ..Default::default()
        };
        def.roles
            .insert("ListNormal".into(), "#123456/#654321".into());
        let theme = expand(&def);
        let s = theme.style(Role::ListNormal);
        assert_eq!(
            (s.fg, s.bg),
            (Color::Rgb(0x12, 0x34, 0x56), Color::Rgb(0x65, 0x43, 0x21))
        );
        // Roles we did not override still follow the slots.
        assert_eq!(theme.style(Role::FrameActive).bg, Color::Rgb(16, 16, 16));
    }

    #[test]
    fn role_names_round_trip() {
        for role in ALL_ROLES {
            assert_eq!(role_by_name(role.name()), Some(role), "{}", role.name());
        }
        assert_eq!(role_by_name("nope"), None);
    }

    #[test]
    fn names_are_presets_plus_sorted_user_themes() {
        let user = ThemeDef {
            background: Some("#000".into()),
            ..Default::default()
        };
        let defs = defs(&[
            ("zeta", user.clone()),
            ("alpha", user.clone()),
            ("dark", user),
        ]);
        assert_eq!(
            theme_names(&defs),
            vec!["classic", "dark", "light", "terminal", "alpha", "zeta"]
        );
        // The user's `dark` wins over the preset definition.
        let resolved = resolve(&defs, "dark");
        assert_eq!(
            resolved.style(Role::Background).bg,
            Color::Rgb(0, 0, 0),
            "user definition replaces the preset"
        );
    }

    #[test]
    fn validation_reports_bad_colours_unknown_roles_and_a_dangling_selection() {
        let mut bad = ThemeDef {
            background: Some("nope".into()),
            ..Default::default()
        };
        bad.roles.insert("NoSuchRole".into(), "#fff/#000".into());
        bad.roles.insert("ListNormal".into(), "#fff".into());
        bad.roles.insert("ListSelected".into(), "nope/#000".into());
        let out = validate(&defs(&[("broken", bad)]), &Some("ghost".into()));
        let fields: Vec<&str> = out.iter().map(|(f, _)| f.as_str()).collect();
        assert!(fields.contains(&"themes.broken.background"), "{out:?}");
        assert!(
            fields.contains(&"themes.broken.roles.NoSuchRole"),
            "{out:?}"
        );
        assert!(
            fields.contains(&"themes.broken.roles.ListNormal"),
            "{out:?}"
        );
        assert!(
            fields.contains(&"themes.broken.roles.ListSelected"),
            "{out:?}"
        );
        assert!(
            fields.contains(&"theme"),
            "dangling selection reported: {out:?}"
        );
        assert!(validate(&HashMap::new(), &None).is_empty(), "clean config");
        assert!(
            validate(&HashMap::new(), &Some("dark".into())).is_empty(),
            "a preset selection is fine"
        );
    }

    #[test]
    fn terminal_preset_is_pure_passthrough() {
        let theme = resolve(&HashMap::new(), "terminal");
        for role in [
            Role::Background,
            Role::FrameActive,
            Role::ListNormal,
            Role::StatusNormal,
            Role::Error,
        ] {
            let s = theme.style(role);
            assert_eq!(s.fg, Color::Default, "{} fg", role.name());
            assert_eq!(s.bg, Color::Default, "{} bg", role.name());
        }
    }

    #[test]
    fn a_serialized_palette_reproduces_itself() {
        // The save-as-theme path: whatever palette is live, dumping it to a
        // def and expanding it again must give back the same RENDERED colors
        // for all 80 roles (classic's Bios variants come back as RGB — the
        // backend resolves both identically, so compare hex, not variants).
        let mut custom = ThemeDef {
            background: Some("#101020".into()),
            foreground: Some("#e0e0f0".into()),
            ..Default::default()
        };
        custom
            .roles
            .insert("FrameActive".into(), "#ff0000/#00ff00".into());
        for original in [
            resolve(&HashMap::new(), "classic"),
            resolve(&HashMap::new(), "dark"),
            resolve(&HashMap::new(), "terminal"),
            expand(&custom),
        ] {
            let round = expand(&theme_to_def(&original));
            for role in ALL_ROLES {
                let want = original.style(role);
                let got = round.style(role);
                assert_eq!(color_hex(got.fg), color_hex(want.fg), "{} fg", role.name());
                assert_eq!(color_hex(got.bg), color_hex(want.bg), "{} bg", role.name());
            }
        }
    }
}
