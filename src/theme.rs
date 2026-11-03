//! Themes: built-in `light`, `dark`, `high-contrast`, `ink`, plus user themes written in TOML
//! (`<config dir>/themes/<name>.toml`). A theme is a set of tokens; each token maps 1:1 to a CSS
//! custom property (`ui.bg` → `--ui-bg`). Only colors (`#rgb`, `#rrggbb`, `#rrggbbaa`), font
//! family lists of safe characters, enumerated keywords and bounded numbers are accepted, so a
//! theme can never inject CSS. Schema: `docs/schema/theme.schema.json`.
//!
//! ```toml
//! name = "solarized-ish"
//! base = "dark"
//! density = "compact"
//!
//! [brand]
//! accent = "#e8590c"
//!
//! [font]
//! ui = "Pretendard, system-ui"
//! size = 13
//! ```
use crate::util::{now_ms, LockExt, Res};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Color,
    Font,
    Size,
    Density,
}

/// (group, key, kind, description). Every token of a built-in theme is defined.
pub const TOKENS: &[(&str, &str, &str, &str)] = &[
    ("brand", "accent", "color", "wordmark speed lines and favicon only"),
    ("brand", "pill", "color", "dark pill behind the wordmark"),
    ("ui", "bg", "color", "page and project group header band"),
    ("ui", "surface", "color", "agent rows, cards, dialogs"),
    ("ui", "subtle", "color", "column headers, footers"),
    ("ui", "fg", "color", "primary text"),
    ("ui", "muted", "color", "secondary text"),
    ("ui", "dim", "color", "tertiary text"),
    ("ui", "border", "color", "1 px separators"),
    ("ui", "border-strong", "color", "group borders"),
    ("ui", "interact", "color", "buttons, focus, selection, links, my message marks"),
    ("ui", "on-interact", "color", "text on primary buttons (drawn on ui.interact)"),
    ("ui", "link", "color", "link and inline action text"),
    ("ui", "selection", "color", "selected row background"),
    ("signal", "ask", "color", "question waiting (needs me, 1st)"),
    ("signal", "ask-bg", "color", "question chip background"),
    ("signal", "ask-text", "color", "question label and chip text"),
    ("signal", "error", "color", "stopped on an error (needs me, 2nd)"),
    ("signal", "error-bg", "color", "error chip background"),
    ("signal", "error-text", "color", "error label and chip text"),
    ("signal", "turn", "color", "my turn / unread answer (needs me, 3rd)"),
    ("signal", "turn-text", "color", "text on turn chips"),
    ("signal", "turn-bg", "color", "turn chip background"),
    ("signal", "decision", "color", "decisions for me"),
    ("info", "cache", "color", "cache countdown (informational)"),
    ("info", "warn", "color", "capacity and held warnings"),
    ("info", "thread", "color", "agent-to-agent threads"),
    ("info", "progress", "color", "progress counters"),
    ("info", "held", "color", "held deliveries"),
    ("timeline", "working", "color", "working bar (drawn opaque)"),
    ("timeline", "idle", "color", "idle track"),
    ("timeline", "snoozed", "color", "snoozed hatch"),
    ("timeline", "now", "color", "now line"),
    ("harness", "omp", "color", "omp glyph tint"),
    ("harness", "cc", "color", "Claude Code glyph tint"),
    ("harness", "cdx", "color", "Codex glyph tint"),
    ("harness", "agy", "color", "Antigravity glyph tint"),
    ("font", "ui", "font", "UI font family list"),
    ("font", "mono", "font", "monospace font family list"),
    ("font", "size", "size", "base font size in px (11 to 18)"),
    ("density", "", "density", "compact | comfortable"),
];

fn kind_of(group: &str, key: &str) -> Option<Kind> {
    TOKENS.iter().find(|t| t.0 == group && t.1 == key).map(|t| match t.2 {
        "color" => Kind::Color,
        "font" => Kind::Font,
        "size" => Kind::Size,
        _ => Kind::Density,
    })
}

/// CSS custom property of a token (`ui.bg` → `--ui-bg`, `density` → `--density`).
pub fn css_var(group: &str, key: &str) -> String {
    if key.is_empty() {
        format!("--{group}")
    } else {
        format!("--{group}-{key}")
    }
}

const LIGHT: &str = r##"name = "light"
density = "comfortable"

[brand]
accent = "#ea580c"
pill = "#0f172a"

[ui]
bg = "#f1f5f9"
surface = "#ffffff"
subtle = "#f8fafc"
fg = "#0f172a"
muted = "#64748b"
dim = "#94a3b8"
border = "#e2e8f0"
border-strong = "#cbd5e1"
interact = "#2563eb"
on-interact = "#ffffff"
link = "#2563eb"
selection = "#2563eb0f"

[signal]
ask = "#6d28d9"
ask-bg = "#f3e8ff"
ask-text = "#6d28d9"
error = "#d32020"
error-bg = "#fef2f2"
error-text = "#d32020"
turn = "#d97706"
turn-text = "#92400e"
turn-bg = "#fffbeb"
decision = "#b45309"

[info]
cache = "#64748b"
warn = "#c2410c"
thread = "#0f766e"
progress = "#64748b"
held = "#a16207"

[timeline]
working = "#96a0b0"
idle = "#cbd5e1"
snoozed = "#64748b"
now = "#2563eb"

[harness]
omp = "#334155"
cc = "#334155"
cdx = "#334155"
agy = "#334155"

[font]
ui = 'Pretendard, system-ui, "IBM Plex Sans KR", "Noto Sans KR", sans-serif'
mono = 'ui-monospace, "JetBrains Mono", monospace'
size = 12.5
"##;

const DARK: &str = r##"name = "dark"
base = "light"

[brand]
accent = "#fb923c"
pill = "#161c28"

[ui]
bg = "#131822"
surface = "#0b0e14"
subtle = "#0f131c"
fg = "#f1f5f9"
muted = "#8b98ad"
dim = "#526077"
border = "#222938"
border-strong = "#333d52"
interact = "#60a5fa"
on-interact = "#0b0e14"
link = "#60a5fa"
selection = "#60a5fa1a"

[signal]
ask = "#c084fc"
ask-bg = "#2e1a47"
ask-text = "#c084fc"
error = "#f87171"
error-bg = "#3b181e"
error-text = "#f87171"
turn = "#fbbf24"
turn-text = "#fde68a"
turn-bg = "#33240d"
decision = "#fbbf24"

[info]
cache = "#8b98ad"
warn = "#fdba74"
thread = "#2dd4bf"
progress = "#8b98ad"
held = "#facc15"

[timeline]
working = "#64748b"
idle = "#232a38"
snoozed = "#8b98ad"
now = "#60a5fa"

[harness]
omp = "#cbd5e1"
cc = "#cbd5e1"
cdx = "#cbd5e1"
agy = "#cbd5e1"
"##;

const HIGH_CONTRAST: &str = r##"name = "high-contrast"
base = "light"

[ui]
bg = "#ffffff"
surface = "#ffffff"
subtle = "#f1f1f1"
fg = "#000000"
muted = "#333333"
dim = "#555555"
border = "#000000"
border-strong = "#000000"
interact = "#0033cc"
link = "#0033cc"
selection = "#dde6ff"
[signal]
ask = "#5b00b3"
ask-text = "#5b00b3"
ask-bg = "#efe0ff"
error = "#b00000"
error-text = "#b00000"
error-bg = "#ffe0e0"
turn = "#8a4b00"
turn-text = "#5c3200"
turn-bg = "#fff0d6"
decision = "#8a4b00"
[info]
cache = "#333333"
warn = "#8a3000"
thread = "#005c55"
progress = "#333333"
held = "#555555"
[timeline]
working = "#8a8a8a"
idle = "#999999"
snoozed = "#333333"
now = "#0033cc"
[harness]
omp = "#000000"
cc = "#000000"
cdx = "#000000"
agy = "#000000"
"##;

const INK: &str = r##"name = "ink"
base = "dark"

[brand]
accent = "#22d3ee"
pill = "#0b0d12"
[ui]
bg = "#0e1116"
surface = "#090b0f"
subtle = "#0c0f14"
fg = "#e7eaf0"
muted = "#8a93a6"
dim = "#555e70"
border = "#1d222c"
border-strong = "#2c3442"
interact = "#38bdf8"
link = "#38bdf8"
selection = "#10283a"
[timeline]
now = "#38bdf8"
"##;

/// Hand-written adash themes.
const ADASH: &[(&str, &str, &str)] =
    &[("light", "adash light", LIGHT), ("dark", "adash dark", DARK), ("high-contrast", "adash high contrast", HIGH_CONTRAST), ("ink", "adash ink", INK)];

/// Every built-in theme as (name, theme file text): the adash themes, then the published
/// palettes derived by [`crate::palette::derive`].
pub fn builtins() -> &'static [(String, String)] {
    static B: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
        let mut v: Vec<(String, String)> = ADASH.iter().map(|(n, l, s)| (n.to_string(), format!("family = \"adash\"\nlabel = \"{l}\"\n{s}"))).collect();
        for src in PALETTES {
            let p: crate::palette::Palette = serde_json::from_str(src).expect("built-in palette parses");
            let d = crate::palette::derive(&p).unwrap_or_else(|e| panic!("built-in palette {}: {e}", p.id));
            v.push((p.id.clone(), crate::palette::to_toml(&p, &d, "")));
        }
        v
    });
    &B
}

fn is_builtin(name: &str) -> bool {
    builtins().iter().any(|b| b.0 == name)
}

/// Published palettes (official values, source in each file), derived to themes by
/// [`crate::palette::derive`].
pub const PALETTES: &[&str] = &[
    include_str!("themes/dracula.json"),
    include_str!("themes/catppuccin-mocha.json"),
    include_str!("themes/catppuccin-latte.json"),
    include_str!("themes/tokyo-night.json"),
    include_str!("themes/tokyo-night-day.json"),
    include_str!("themes/nord.json"),
    include_str!("themes/gruvbox-dark.json"),
    include_str!("themes/gruvbox-light.json"),
    include_str!("themes/solarized-dark.json"),
    include_str!("themes/solarized-light.json"),
    include_str!("themes/rose-pine.json"),
    include_str!("themes/rose-pine-moon.json"),
    include_str!("themes/rose-pine-dawn.json"),
    include_str!("themes/kanagawa-wave.json"),
    include_str!("themes/kanagawa-lotus.json"),
    include_str!("themes/everforest-dark.json"),
    include_str!("themes/everforest-light.json"),
    include_str!("themes/github-dark.json"),
    include_str!("themes/github-dark-dimmed.json"),
    include_str!("themes/github-light.json"),
    include_str!("themes/modus-vivendi.json"),
    include_str!("themes/modus-operandi.json"),
    include_str!("themes/one-dark.json"),
    include_str!("themes/one-light.json"),
    include_str!("themes/ayu-dark.json"),
    include_str!("themes/ayu-mirage.json"),
    include_str!("themes/ayu-light.json"),
    include_str!("themes/flexoki-dark.json"),
    include_str!("themes/flexoki-light.json"),
    include_str!("themes/nightfox.json"),
    include_str!("themes/dayfox.json"),
    include_str!("themes/dawnfox.json"),
    include_str!("themes/duskfox.json"),
    include_str!("themes/nordfox.json"),
    include_str!("themes/terafox.json"),
    include_str!("themes/carbonfox.json"),
    include_str!("themes/okabe-ito-dark.json"),
    include_str!("themes/okabe-ito-light.json"),
];

#[derive(Clone, Debug, Default)]
pub struct Theme {
    pub name: String,
    /// "builtin" | "user"
    pub source: &'static str,
    pub path: Option<PathBuf>,
    pub base: Option<String>,
    /// "group.key" → value (after inheritance)
    pub tokens: BTreeMap<String, String>,
    pub warnings: Vec<String>,
    /// errors make the theme fall back to its base
    pub errors: Vec<String>,
    /// picker group: "adash", a palette family ("dracula", …) or "user"
    pub family: String,
    /// display name
    pub label: String,
    /// the same family's theme of the opposite brightness
    pub pair: Option<String>,
}

impl Theme {
    pub fn dark(&self) -> bool {
        self.tokens.get("ui.surface").and_then(|c| luminance(c)).is_some_and(|l| l < 0.2)
    }
}

fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let before: String = src.chars().take(offset).collect();
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
    (line, col)
}

fn valid_color(v: &str) -> bool {
    let h = v.strip_prefix('#').unwrap_or("");
    matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit()) && v.starts_with('#')
}

const GENERIC_FONTS: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "math",
    "emoji",
    "fangsong",
];

/// Parse a font-family list (`Inter, "Noto Sans KR", sans-serif`) and serialize it for CSS:
/// every family double-quoted, generic families bare. A family is letters, digits, spaces and
/// `. _ -`, optionally wrapped in matching quotes; empty items and stray quotes are errors.
pub fn font_list(v: &str) -> Result<String, String> {
    if v.chars().count() > 200 {
        return Err("font list longer than 200 characters".into());
    }
    let mut out = vec![];
    for item in v.split(',') {
        let item = item.trim();
        let (inner, quoted) = match item.as_bytes() {
            [q @ (b'"' | b'\''), .., l] if q == l && item.len() >= 2 => (&item[1..item.len() - 1], true),
            _ => (item, false),
        };
        let inner = inner.trim();
        if inner.is_empty() {
            return Err("empty font name in the list".into());
        }
        if let Some(c) = inner.chars().find(|c| !(c.is_alphanumeric() || " ._-".contains(*c))) {
            return Err(format!(
                "font name {item:?}: character {c:?} is not allowed (letters, digits, spaces and . _ - only; quotes must wrap the whole name)"
            ));
        }
        out.push(if !quoted && GENERIC_FONTS.contains(&inner) { inner.to_string() } else { format!("\"{inner}\"") });
    }
    Ok(out.join(", "))
}

/// Parse one theme document (raw tokens, before inheritance): (name, base, tokens, warnings,
/// errors). Syntax errors carry line:col; value errors name the token.
fn parse(src: &str) -> (Option<String>, Option<String>, BTreeMap<String, String>, Vec<String>, Vec<String>) {
    let mut errors = vec![];
    let mut warnings = vec![];
    let mut tokens = BTreeMap::new();
    let doc: toml::Table = match src.parse() {
        Ok(d) => d,
        Err(e) => {
            let e: toml::de::Error = e;
            let pos = e.span().map(|s| {
                let (l, c) = line_col(src, src[..s.start.min(src.len())].chars().count());
                format!("{l}:{c}: ")
            });
            errors.push(format!("{}{}", pos.unwrap_or_default(), e.message()));
            return (None, None, tokens, warnings, errors);
        }
    };
    let name = doc.get("name").and_then(|v| v.as_str()).map(String::from);
    let base = doc.get("base").and_then(|v| v.as_str()).map(String::from);
    for (group, v) in &doc {
        match (group.as_str(), v) {
            ("name" | "base" | "$schema" | "family" | "label", _) => {}
            ("density", toml::Value::String(d)) if matches!(d.as_str(), "compact" | "comfortable") => {
                tokens.insert("density".into(), d.clone());
            }
            ("density", other) => errors.push(format!("density must be \"compact\" or \"comfortable\", not {other}")),
            (g, toml::Value::Table(t)) if TOKENS.iter().any(|x| x.0 == g) => {
                for (key, val) in t {
                    let Some(kind) = kind_of(g, key) else {
                        warnings.push(format!("unknown token {g}.{key} (ignored)"));
                        continue;
                    };
                    let ok = match kind {
                        Kind::Color => match val.as_str().filter(|s| valid_color(s)).map(|s| s.to_ascii_lowercase()) {
                            Some(c) if alpha(&c) < 255 && !ALPHA_OK.contains(&format!("{g}.{key}").as_str()) => {
                                errors.push(format!("{g}.{key}: transparency is not allowed (only {} may be translucent)", ALPHA_OK.join(", ")));
                                continue;
                            }
                            c => c,
                        },
                        Kind::Font => match val.as_str().map(font_list) {
                            Some(Ok(f)) => Some(f),
                            Some(Err(e)) => {
                                errors.push(format!("{g}.{key}: {e}"));
                                continue;
                            }
                            None => None,
                        },
                        Kind::Size => val.as_integer().map(|i| i as f64).or(val.as_float()).filter(|f| (11.0..=18.0).contains(f)).map(|f| format!("{f}")),
                        Kind::Density => None,
                    };
                    match ok {
                        Some(s) => {
                            tokens.insert(format!("{g}.{key}"), s);
                        }
                        None => errors.push(format!(
                            "{g}.{key} = {val}: {}",
                            match kind {
                                Kind::Color => "expected a color \"#rgb\", \"#rrggbb\" or \"#rrggbbaa\"",
                                Kind::Font => "expected a font list string",
                                Kind::Size => "expected a number between 11 and 18",
                                Kind::Density => "invalid",
                            }
                        )),
                    }
                }
            }
            (g, _) => warnings.push(format!("unknown key {g} (ignored)")),
        }
    }
    (name, base, tokens, warnings, errors)
}

// ---- contrast

/// Tokens that may carry an alpha channel; they are composited over `ui.surface` before any
/// contrast is computed. Every other color must be opaque.
const ALPHA_OK: &[&str] = &[
    "ui.selection",
    "ui.border",
    "ui.border-strong",
    "signal.ask-bg",
    "signal.error-bg",
    "signal.turn-bg",
    "timeline.idle",
    // secondary text: a palette's foreground at reduced alpha (themes with one text color)
    "ui.muted",
    "ui.dim",
    "info.cache",
    "info.progress",
    "info.held",
    "harness.omp",
    "harness.cc",
    "harness.cdx",
    "harness.agy",
];

fn rgba(c: &str) -> Option<(f64, f64, f64, f64)> {
    let h = c.strip_prefix('#')?;
    let h: String = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect() } else { h.to_string() };
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|v| v as f64 / 255.0);
    Some((p(0)?, p(2)?, p(4)?, if h.len() == 8 { p(6)? } else { 1.0 }))
}

fn alpha(c: &str) -> u8 {
    rgba(c).map(|x| (x.3 * 255.0).round() as u8).unwrap_or(255)
}

/// `c` composited over the opaque color `under`.
fn over(c: &str, under: &str) -> Option<(f64, f64, f64)> {
    let (r, g, b, a) = rgba(c)?;
    let (ur, ug, ub, _) = rgba(under)?;
    Some((r * a + ur * (1.0 - a), g * a + ug * (1.0 - a), b * a + ub * (1.0 - a)))
}

fn lum((r, g, b): (f64, f64, f64)) -> f64 {
    let lin = |v: f64| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn luminance(c: &str) -> Option<f64> {
    rgba(c).map(|(r, g, b, _)| lum((r, g, b)))
}

/// WCAG contrast ratio of two opaque colors.
pub fn contrast(a: &str, b: &str) -> Option<f64> {
    ratio(luminance(a)?, luminance(b)?)
}

fn ratio(la: f64, lb: f64) -> Option<f64> {
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// Text drawn on a background, as the dashboard draws it: (foreground, background).
const TEXT_PAIRS: &[(&str, &str)] = &[
    ("ui.fg", "ui.surface"),
    ("ui.fg", "ui.bg"),
    ("ui.fg", "ui.subtle"),
    ("ui.fg", "ui.selection"),
    ("ui.muted", "ui.surface"),
    ("ui.link", "ui.surface"),
    ("signal.ask-text", "signal.ask-bg"),
    ("signal.ask-text", "ui.surface"),
    ("signal.error-text", "signal.error-bg"),
    ("signal.error-text", "ui.surface"),
    ("signal.turn-text", "signal.turn-bg"),
    ("signal.turn-text", "ui.surface"),
    ("#ffffff", "brand.pill"),
    ("signal.decision", "ui.surface"),
    ("info.cache", "ui.surface"),
    ("info.warn", "ui.surface"),
    ("info.thread", "ui.surface"),
    ("info.progress", "ui.surface"),
];
/// Bars and marks (non-text) on the card surface.
const MARK_PAIRS: &[(&str, &str)] = &[
    ("signal.ask", "ui.surface"),
    ("signal.error", "ui.surface"),
    ("signal.turn", "ui.surface"),
    ("timeline.now", "ui.surface"),
    ("ui.interact", "ui.surface"),
];
/// Text on a solid fill (primary buttons): 11.5 px, regular weight, so the text minimum applies.
const BUTTON_PAIRS: &[(&str, &str)] = &[("ui.on-interact", "ui.interact")];
/// Text (button text too): error under 3:1, warning under 4.5:1. Marks: error under 2:1,
/// warning under 3:1.
const TEXT_MIN: (f64, f64) = (3.0, 4.5);
const MARK_MIN: (f64, f64) = (2.0, 3.0);

/// Contrast of the pairs the dashboard actually draws (translucent backgrounds composited over
/// `ui.surface`, then the foreground over that), and the solid needs-me bars (question, error)
/// against the working bar in grayscale (warning under 1.5:1; the turn bar is a hollow rail,
/// told apart by shape).
fn check(tokens: &BTreeMap<String, String>) -> (Vec<String>, Vec<String>) {
    let (mut warn, mut err) = (vec![], vec![]);
    let t = |k: &str| tokens.get(k).map(String::as_str);
    let Some(surface) = t("ui.surface") else {
        err.push("ui.surface is not set".into());
        return (warn, err);
    };
    // a pair names tokens, or a literal color the stylesheet draws with (the wordmark's white)
    let pair = |fg: &'static str, bg: &'static str| -> Option<f64> {
        let t = |k: &'static str| if k.starts_with('#') { Some(k) } else { t(k) };
        let bg_rgb = over(t(bg)?, surface)?;
        let bg_hex = format!("#{:02x}{:02x}{:02x}", (bg_rgb.0 * 255.0).round() as u8, (bg_rgb.1 * 255.0).round() as u8, (bg_rgb.2 * 255.0).round() as u8);
        ratio(lum(over(t(fg)?, &bg_hex)?), lum(bg_rgb))
    };
    for (pairs, (e, w), what) in [(TEXT_PAIRS, TEXT_MIN, "text"), (MARK_PAIRS, MARK_MIN, "marks"), (BUTTON_PAIRS, TEXT_MIN, "text")] {
        for (fg, bg) in pairs {
            let Some(c) = pair(fg, bg) else {
                err.push(format!("{fg} on {bg}: color missing"));
                continue;
            };
            let msg = format!("{fg} on {bg}: contrast {c:.2}:1");
            if c < e {
                err.push(format!("{msg} ({what} needs at least {e}:1)"));
            } else if c < w {
                warn.push(format!("{msg} ({what} needs {w}:1)"));
            }
        }
    }
    if let Some(work) = t("timeline.working") {
        for k in ["signal.ask", "signal.error"] {
            if let Some(c) = t(k).and_then(|v| contrast(v, work)) {
                if c < 1.5 {
                    warn.push(format!("{k} and timeline.working look alike in grayscale ({c:.2}:1)"));
                }
            }
        }
    }
    (warn, err)
}

// ---- loading

pub fn user_dir() -> PathBuf {
    crate::platform::config_dir().join("themes")
}

type Raw = (Option<String>, Option<String>, BTreeMap<String, String>, Vec<String>, Vec<String>);

/// Resolve `name` from theme sources: the base is resolved (and validated) first, the theme's own
/// tokens go on top, then the complete token set is checked. On any error — syntax, bad value,
/// missing/self/cyclic base, contrast, an incomplete set — the theme shows its validated base
/// (or `light` when the base itself cannot be resolved).
struct Resolved {
    tokens: BTreeMap<String, String>,
    warnings: Vec<String>,
    errors: Vec<String>,
    /// inheritance errors (self, cycle, depth, missing base) of this theme or any ancestor; every
    /// descendant reports them too
    chain: Vec<String>,
    base: Option<String>,
}

fn resolve(name: &str, srcs: &BTreeMap<String, String>, stack: &mut Vec<String>) -> Resolved {
    let src = srcs.get(name).map(String::as_str).unwrap_or_default();
    let (_, base, own, mut warnings, mut errors): Raw = parse(src);
    let base_name = base.clone().or_else(|| (name != "light").then(|| "light".to_string()));
    stack.push(name.to_string());
    let mut chain = vec![];
    let mut parent: Option<BTreeMap<String, String>> = None;
    if let Some(b) = &base_name {
        if b == name {
            chain.push(format!("base \"{b}\" is the theme itself"));
        } else if stack.contains(b) {
            chain.push(format!("base cycle: {} → {b}", stack.join(" → ")));
        } else if stack.len() > 8 {
            chain.push("base chain deeper than 8 themes".into());
        } else if !srcs.contains_key(b) {
            chain.push(format!("base theme {b} not found"));
        } else {
            let p = resolve(b, srcs, stack);
            chain.extend(p.chain.into_iter().map(|e| if e.starts_with("via ") { e } else { format!("via {b}: {e}") }));
            parent = Some(p.tokens);
        }
    }
    stack.pop();
    errors.extend(chain.iter().cloned());
    let fallback = |parent: Option<BTreeMap<String, String>>| {
        parent.unwrap_or_else(|| if name == "light" { BTreeMap::new() } else { resolve("light", srcs, &mut vec![]).tokens })
    };
    let done = |tokens, warnings, errors| Resolved { tokens, warnings, errors, chain: chain.clone(), base: base.clone() };
    if !errors.is_empty() {
        return done(fallback(parent), warnings, errors);
    }
    let mut tokens = parent.clone().unwrap_or_default();
    tokens.extend(own);
    for (g, k, _, _) in TOKENS {
        let key = if k.is_empty() { g.to_string() } else { format!("{g}.{k}") };
        if !tokens.contains_key(&key) {
            errors.push(format!("{key} is not set (the base chain must end at a complete theme)"));
        }
    }
    let (w, e) = check(&tokens);
    warnings.extend(w);
    errors.extend(e);
    if !errors.is_empty() {
        return done(fallback(parent), warnings, errors);
    }
    done(tokens, warnings, errors)
}

fn build_from(name: &str, source: &'static str, path: Option<PathBuf>, srcs: &BTreeMap<String, String>) -> Theme {
    let r = resolve(name, srcs, &mut vec![]);
    let meta: toml::Table = srcs.get(name).and_then(|s| s.parse().ok()).unwrap_or_default();
    let str_of = |k: &str| meta.get(k).and_then(|v| v.as_str()).map(String::from);
    let family = if source == "user" { "user".to_string() } else { str_of("family").unwrap_or_else(|| "adash".into()) };
    let label = str_of("label").unwrap_or_else(|| name.to_string());
    Theme { name: name.to_string(), source, path, base: r.base, tokens: r.tokens, warnings: r.warnings, errors: r.errors, family, label, pair: None }
}

fn builtin_srcs() -> BTreeMap<String, String> {
    builtins().iter().cloned().collect()
}

/// User theme files: (name, path, source, problem). A file whose name is a built-in's is refused.
fn user_files() -> Vec<(String, PathBuf, String, Option<String>)> {
    let Ok(rd) = std::fs::read_dir(user_dir()) else { return vec![] };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let src = std::fs::read_to_string(&p).unwrap_or_default();
            let name = parse(&src).0.or_else(|| p.file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_default();
            let problem = if !valid_name(&name) {
                Some(format!("theme name {name:?}: use letters, digits, - and _"))
            } else if is_builtin(&name) {
                Some(format!("\"{name}\" is a built-in theme; give your theme another name"))
            } else {
                None
            };
            (name, p, src, problem)
        })
        .collect()
}

/// All themes: built-ins, then valid user files; plus the refused files with the reason.
pub fn load_all_with_refused() -> (Vec<Theme>, Vec<(PathBuf, String)>) {
    let mut srcs = builtin_srcs();
    let mut users = vec![];
    let mut refused = vec![];
    for (name, path, src, problem) in user_files() {
        match problem {
            Some(p) => refused.push((path, p)),
            None if users.iter().any(|(n, _): &(String, PathBuf)| n == &name) => refused.push((path, format!("another file already defines theme \"{name}\""))),
            None => {
                srcs.insert(name.clone(), src);
                users.push((name, path));
            }
        }
    }
    let mut out: Vec<Theme> = builtins().iter().map(|(n, _)| build_from(n, "builtin", None, &srcs)).collect();
    out.extend(users.into_iter().map(|(n, p)| build_from(&n, "user", Some(p), &srcs)));
    // the same family's first theme of the opposite brightness
    let keys: Vec<(String, String, bool)> = out.iter().map(|t| (t.name.clone(), t.family.clone(), t.dark())).collect();
    for t in &mut out {
        if t.family != "user" {
            t.pair = keys.iter().find(|(n, f, d)| f == &t.family && *d != t.dark() && n != &t.name).map(|k| k.0.clone());
        }
    }
    (out, refused)
}

pub fn load_all() -> Vec<Theme> {
    load_all_with_refused().0
}

pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Generated stylesheet for one theme: CSS custom properties on `:root`.
pub fn css(t: &Theme) -> String {
    let mut s = format!("/* adash theme {} ({}) */\n:root {{\n", t.name, t.source);
    for (k, v) in &t.tokens {
        let (g, key) = k.split_once('.').unwrap_or((k.as_str(), ""));
        // fonts are lists of safe characters; numbers become px
        let val = if g == "font" && key == "size" { format!("{v}px") } else { v.clone() };
        s.push_str(&format!("  {}: {val};\n", css_var(g, key)));
    }
    s.push_str("}\n");
    s
}

/// Picker color strip: page, card, text, question, error, my turn, interaction.
const SWATCH: [&str; 7] = ["ui.bg", "ui.surface", "ui.fg", "signal.ask", "signal.error", "signal.turn", "ui.interact"];

pub fn view(t: &Theme) -> Value {
    json!({
        "name": t.name, "source": t.source, "path": t.path, "base": t.base,
        "dark": t.dark(), "family": t.family, "label": t.label, "pair": t.pair,
        "swatch": SWATCH.map(|k| t.tokens.get(k).cloned().unwrap_or_default()),
        "warnings": t.warnings, "errors": t.errors,
    })
}

/// Theme list cached with a change signature of the user directory; `version` changes when a
/// theme file is saved (the UI reloads the stylesheet).
static CACHE: LazyLock<Mutex<(String, i64, Vec<Theme>)>> = LazyLock::new(Default::default);

fn dir_signature() -> String {
    let Ok(rd) = std::fs::read_dir(user_dir()) else { return String::new() };
    let mut v: Vec<String> = rd.flatten().map(|e| format!("{}:{}", e.file_name().to_string_lossy(), crate::util::mtime_ms(&e.path()))).collect();
    v.sort();
    v.join("|")
}

/// Themes, re-read when the user directory changed. `force`: re-read now.
pub fn themes(force: bool) -> (i64, Vec<Theme>) {
    let sig = dir_signature();
    let mut c = CACHE.plock();
    if force || c.2.is_empty() || c.0 != sig {
        *c = (sig, now_ms(), load_all());
    }
    (c.1, c.2.clone())
}

pub fn get(name: &str) -> Option<Theme> {
    themes(false).1.into_iter().find(|t| t.name == name)
}

/// Commented template for `adash theme new` (taplo reads the `#:schema` line).
pub fn template(name: &str, from: &str) -> String {
    let mut s = format!(
        "#:schema {SCHEMA_URL}\n# adash theme. Save as {}/{name}.toml; the dashboard reloads it on save.\n# Colors: \"#rgb\", \"#rrggbb\" or \"#rrggbbaa\". Unset tokens come from the base theme.\nname = \"{name}\"\nbase = \"{from}\"\n# density = \"comfortable\"   # compact | comfortable\n",
        user_dir().display()
    );
    let mut group = "";
    for (g, k, _, d) in TOKENS {
        if k.is_empty() {
            continue;
        }
        if *g != group {
            group = g;
            s.push_str(&format!("\n[{g}]\n"));
        }
        s.push_str(&format!("# {k} = …   # {d}\n"));
    }
    s
}

/// JSON Schema location for editors (taplo `#:schema`).
pub const SCHEMA_URL: &str = "https://raw.githubusercontent.com/Axect/adash/main/docs/schema/theme.schema.json";

/// JSON Schema of theme files, generated from [`TOKENS`].
pub fn schema() -> Value {
    let color = json!({"type": "string", "pattern": "^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$"});
    // an 8-digit color is opaque only with alpha ff
    let opaque = json!({"type": "string", "pattern": "^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{6}[fF]{2})$"});
    let mut props = serde_json::Map::new();
    props.insert("$schema".into(), json!({"type": "string"}));
    props.insert("name".into(), json!({"type": "string", "pattern": "^[A-Za-z0-9_-]{1,40}$"}));
    props.insert("base".into(), json!({"type": "string", "description": "theme to inherit from (light, dark, high-contrast, ink or a user theme)"}));
    props.insert("density".into(), json!({"enum": ["compact", "comfortable"]}));
    props.insert("family".into(), json!({"type": "string", "description": "picker group (user themes are listed under \"user\")"}));
    props.insert("label".into(), json!({"type": "string", "description": "display name"}));
    let mut groups: BTreeMap<&str, serde_json::Map<String, Value>> = BTreeMap::new();
    for (g, k, kind, d) in TOKENS {
        if k.is_empty() {
            continue;
        }
        let mut p = match *kind {
            "color" if ALPHA_OK.contains(&format!("{g}.{k}").as_str()) => color.clone(),
            "color" => opaque.clone(),
            "font" => {
                // same rule as font_list(): comma-separated names, each optionally wrapped in matching quotes
                let name = r"[\p{L}\p{N}._-][\p{L}\p{N} ._-]*";
                let item = format!(r#"\s*(?:"{name}"|'{name}'|{name})\s*"#);
                json!({"type": "string", "maxLength": 200, "pattern": format!("^{item}(?:,{item})*$")})
            }
            _ => json!({"type": "number", "minimum": 11, "maximum": 18}),
        };
        p["description"] = json!(d);
        groups.entry(g).or_default().insert(k.to_string(), p);
    }
    for (g, p) in groups {
        props.insert(g.into(), json!({"type": "object", "additionalProperties": false, "properties": p}));
    }
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "title": "adash theme", "type": "object", "additionalProperties": false, "properties": props})
}

/// Warnings and errors of a theme text resolved against the built-ins (importer checks).
pub fn check_text(name: &str, text: &str) -> (Vec<String>, Vec<String>) {
    let mut srcs = builtin_srcs();
    srcs.insert(name.to_string(), text.to_string());
    let t = build_from(name, "user", None, &srcs);
    (t.warnings, t.errors)
}

/// `adash theme check <file>`: errors and warnings for one file (the installed themes are the
/// possible bases).
pub fn check_file(path: &Path) -> Res<Theme> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut srcs = builtin_srcs();
    for (n, p, s, problem) in user_files() {
        if problem.is_none() && p != path {
            srcs.insert(n, s);
        }
    }
    let name = parse(&src).0.unwrap_or_else(|| "unnamed".into());
    let mut t = if is_builtin(&name) {
        let mut t = Theme { name: name.clone(), source: "user", path: Some(path.to_path_buf()), ..Default::default() };
        t.errors.push(format!("\"{name}\" is a built-in theme; give your theme another name"));
        t
    } else {
        srcs.insert(name.clone(), src);
        build_from(&name, "user", Some(path.to_path_buf()), &srcs)
    };
    if !valid_name(&name) {
        t.errors.push(format!("theme name {name:?}: use letters, digits, - and _"));
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `name` from `src` with the built-ins (and `extra` sources) as possible bases.
    fn user(name: &str, src: &str, extra: &[(&str, &str)]) -> Theme {
        let mut srcs = builtin_srcs();
        for (n, s) in extra {
            srcs.insert(n.to_string(), s.to_string());
        }
        srcs.insert(name.into(), src.into());
        build_from(name, "user", None, &srcs)
    }

    fn builtin(name: &str) -> Theme {
        build_from(name, "builtin", None, &builtin_srcs())
    }

    #[test]
    fn builtins_are_complete_and_pass_every_check() {
        for (n, _) in builtins() {
            let t = builtin(n);
            assert!(t.errors.is_empty() && t.warnings.is_empty(), "{n}: {:?} {:?}", t.errors, t.warnings);
            assert_eq!(t.tokens.len(), TOKENS.len(), "{n}");
        }
    }

    /// The stylesheet's fallback values (used before the server CSS arrives) are the light and
    /// dark built-ins: one definition, checked here.
    #[test]
    fn stylesheet_fallback_matches_the_light_and_dark_builtins() {
        let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/style.css")).unwrap();
        let block = |start: &str| {
            let i = css.find(start).unwrap_or_else(|| panic!("{start} missing in style.css"));
            let j = css[i..].find("\n}").unwrap() + i;
            css[i..j].to_string()
        };
        let norm = |k: &str, v: &str| -> String {
            let v = v.trim();
            if let Some(r) = v.strip_prefix("rgba(").and_then(|r| r.strip_suffix(')')) {
                let p: Vec<f64> = r.split(',').map(|x| x.trim().parse().unwrap()).collect();
                return format!("#{:02x}{:02x}{:02x}{:02x}", p[0] as u8, p[1] as u8, p[2] as u8, (p[3] * 255.0).round() as u8);
            }
            match k {
                "font.size" => v.trim_end_matches("px").to_string(),
                "font.ui" | "font.mono" => font_list(v).unwrap(),
                _ => v.to_ascii_lowercase(),
            }
        };
        for (name, start, partial) in [("light", ":root {", false), ("dark", ":root[data-scheme=\"dark\"] {", true)] {
            let t = builtin(name);
            let b = block(start);
            for (g, k, _, _) in TOKENS {
                let key = if k.is_empty() { g.to_string() } else { format!("{g}.{k}") };
                let var = css_var(g, k);
                let found = b.split(';').find_map(|d| {
                    let (n, v) = d.rsplit_once(':')?;
                    (n.trim().rsplit(['{', '\n', ' ']).next()? == var).then(|| v.to_string())
                });
                match found {
                    Some(v) => assert_eq!(norm(&key, &v), t.tokens[&key], "{name} {var}"),
                    None => assert!(partial && (g == &"font" || g == &"density"), "{name}: {var} missing in style.css"),
                }
            }
        }
    }

    #[test]
    fn user_theme_inherits_and_maps_to_css_variables() {
        let t = user("solar", "name = \"solar\"\nbase = \"dark\"\ndensity = \"compact\"\n[brand]\naccent = \"#E8590C\"\n[font]\nui = \"Inter, 'Noto Sans KR', system-ui\"\nsize = 14\n", &[]);
        assert!(t.errors.is_empty(), "{:?}", t.errors);
        assert_eq!(t.tokens["brand.accent"], "#e8590c");
        assert_eq!(t.tokens["ui.surface"], builtin("dark").tokens["ui.surface"], "from dark");
        assert_eq!(t.tokens["font.mono"], builtin("light").tokens["font.mono"], "dark inherits light's fonts");
        let css = css(&t);
        assert!(css.contains("--brand-accent: #e8590c;") && css.contains("--font-size: 14px;") && css.contains("--density: compact;"));
        assert!(css.contains("--font-ui: \"Inter\", \"Noto Sans KR\", system-ui;"), "{css}");
    }

    #[test]
    fn font_lists_are_parsed_and_cannot_spill_into_other_declarations() {
        assert_eq!(
            font_list("Pretendard, system-ui, \"IBM Plex Sans KR\", sans-serif").unwrap(),
            "\"Pretendard\", system-ui, \"IBM Plex Sans KR\", sans-serif"
        );
        assert_eq!(font_list("'monospace'").unwrap(), "\"monospace\"", "a quoted generic is a family name");
        assert_eq!(font_list("나눔고딕, serif").unwrap(), "\"나눔고딕\", serif");
        for bad in ["Iosevka'", "'Iosevka", "\"A\"B\"", "A,,B", "A, ", "Arial; color:red", "A}", "A\\'", ""] {
            assert!(font_list(bad).is_err(), "{bad:?} accepted");
        }
        // the JSON Schema pattern agrees with the server
        let pat = regex::Regex::new(schema()["properties"]["font"]["properties"]["ui"]["pattern"].as_str().unwrap()).unwrap();
        for v in [
            "Pretendard, system-ui, \"IBM Plex Sans KR\", sans-serif",
            "'monospace'",
            "나눔고딕, serif",
            "Iosevka'",
            "'Iosevka",
            "\"A\"B\"",
            "A,,B",
            "A, ",
            "Arial; color:red",
            "A}",
        ] {
            assert_eq!(pat.is_match(v), font_list(v).is_ok(), "schema and server disagree on {v:?}");
        }
        let t = user("f", "[font]\nmono = \"Iosevka'\"\nsize = 18\n[ui]\nfg = \"#000000\"\n", &[]);
        assert!(t.errors.iter().any(|e| e.starts_with("font.mono")), "{:?}", t.errors);
        assert!(!css(&t).contains("Iosevka"));
        // every value of every built-in is one line with balanced quotes and no ; { }
        for (n, _) in builtins() {
            for line in css(&builtin(n)).lines().filter(|l| l.trim_start().starts_with("--")) {
                let v = line.split_once(':').unwrap().1.trim().strip_suffix(';').unwrap();
                assert!(!v.contains([';', '{', '}', '\'']) && v.matches('"').count() % 2 == 0, "{n}: {line}");
            }
        }
    }

    #[test]
    fn bad_values_are_rejected_and_fall_back_to_the_validated_base() {
        let src = "name = \"x\"\nbase = \"dark\"\ndensity = \"tiny\"\n[ui]\nbg = \"red; } body { display:none\"\nfg = \"#12\"\n[font]\nui = \"a</style>\"\nsize = 40\n";
        let t = user("x", src, &[]);
        assert_eq!(t.errors.len(), 5, "{:?}", t.errors);
        assert_eq!(t.tokens, builtin("dark").tokens, "falls back to the validated base");
        let bad = user("y", "name = \"y\"\n[ui]\nbg = \n", &[]);
        assert!(bad.errors.first().is_some_and(|e| e.starts_with("3:")), "{:?}", bad.errors);
    }

    #[test]
    fn a_broken_parent_never_leaks_into_its_child() {
        let parent = "name = \"p\"\n[ui]\nfg = \"#ffffff\"\n";
        let p = user("p", parent, &[]);
        assert!(!p.errors.is_empty());
        let c = user("c", "name = \"c\"\nbase = \"p\"\n[brand]\naccent = \"#123456\"\n", &[("p", parent)]);
        assert_eq!(c.tokens["ui.fg"], builtin("light").tokens["ui.fg"], "child inherits p's fallback, not p's raw white text");
    }

    #[test]
    fn transparency_is_refused_or_composited() {
        let t = user("t", "[ui]\nfg = \"#00000000\"\n", &[]);
        assert!(t.errors.iter().any(|e| e.contains("ui.fg: transparency")), "{:?}", t.errors);
        assert_eq!(t.tokens["ui.fg"], builtin("light").tokens["ui.fg"]);
        // translucent backgrounds are composited: a 10 % violet behind violet text is not 1:1
        let ok = user("s", "[signal]\nask-bg = \"#6d28d91a\"\n", &[]);
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
        let solid = user("s2", "[signal]\nask-bg = \"#6d28d9ff\"\n", &[]);
        assert!(solid.errors.iter().any(|e| e.starts_with("signal.ask-text on signal.ask-bg")), "{:?}", solid.errors);
    }

    #[test]
    fn contrast_is_checked_on_the_pairs_actually_drawn() {
        assert!((contrast("#000000", "#ffffff").unwrap() - 21.0).abs() < 0.01);
        let same = user("q", "[signal]\nask-bg = \"#6d28d9\"\n", &[]);
        assert!(same.errors.iter().any(|e| e.starts_with("signal.ask-text on signal.ask-bg: contrast 1.00:1")), "{:?}", same.errors);
        let low = user("low", "[ui]\nfg = \"#f0f0f0\"\n", &[]);
        assert!(low.errors.iter().any(|e| e.starts_with("ui.fg on ui.surface")), "{:?}", low.errors);
        // 3:1 <= text < 4.5:1 is a warning, the theme is used
        let meh = user("meh", "[ui]\nmuted = \"#7c8799\"\n", &[]);
        assert!(meh.errors.is_empty() && meh.warnings.iter().any(|w| w.starts_with("ui.muted on ui.surface")), "{:?} {:?}", meh.errors, meh.warnings);
        assert_ne!(meh.tokens["ui.muted"], builtin("light").tokens["ui.muted"]);
    }

    #[test]
    fn self_cyclic_and_missing_bases_are_errors() {
        let me = user("me", "name = \"me\"\nbase = \"me\"\n[ui]\nfg = \"#fff\"\n", &[]);
        assert!(me.errors.iter().any(|e| e.contains("is the theme itself")), "{:?}", me.errors);
        assert_eq!(me.tokens, builtin("light").tokens);
        let p = "name = \"p\"\nbase = \"q\"\n";
        let q = "name = \"q\"\nbase = \"p\"\n";
        let t = user("p", p, &[("q", q)]);
        assert!(t.errors.iter().any(|e| e.contains("base cycle")), "{:?}", t.errors);
        assert_eq!(t.tokens.len(), TOKENS.len(), "a cycle still shows a complete theme");
        let none = user("a", "base = \"nope\"\n", &[]);
        assert!(none.errors.iter().any(|e| e.contains("nope")));
    }

    #[test]
    fn template_parses_and_schema_covers_every_token() {
        let t = template("mine", "dark");
        let (name, base, tokens, w, e) = parse(&t);
        assert_eq!((name.as_deref(), base.as_deref(), tokens.len(), w.len(), e.len()), (Some("mine"), Some("dark"), 0, 0, 0));
        let sch = schema();
        for (g, k, _, _) in TOKENS {
            if !k.is_empty() {
                assert!(sch["properties"][*g]["properties"][*k].is_object(), "{g}.{k}");
            }
        }
    }

    #[test]
    fn component_pairs_catch_invisible_turn_tags_and_buttons() {
        let turn = user("rt", "[signal]\nturn-text = \"#ffffff\"\nturn-bg = \"#000000\"\n", &[]);
        assert!(turn.errors.iter().any(|e| e.starts_with("signal.turn-text on ui.surface")), "{:?}", turn.errors);
        // a light accent under the light theme's white button text
        let button = user("rb", "[ui]\ninteract = \"#e0e7ff\"\n", &[]);
        assert!(button.errors.iter().any(|e| e.starts_with("ui.on-interact on ui.interact")), "{:?}", button.errors);
        let fixed = user("rb2", "base = \"dark\"\n[ui]\ninteract = \"#ffffff\"\n", &[]);
        assert!(!fixed.errors.iter().any(|e| e.starts_with("ui.on-interact")), "dark text on a white accent is readable: {:?}", fixed.errors);
        let pill = user("rp", "[brand]\npill = \"#f8f8f8\"\n", &[]);
        assert!(pill.errors.iter().any(|e| e.starts_with("#ffffff on brand.pill")), "{:?}", pill.errors);
    }

    #[test]
    fn inheritance_errors_reach_every_descendant() {
        let extra = [("p", "name = \"p\"\nbase = \"gone\"\n"), ("c", "name = \"c\"\nbase = \"p\"\n")];
        let g = user("g", "name = \"g\"\nbase = \"c\"\n", &extra);
        assert!(g.errors.iter().any(|e| e.contains("base theme gone not found")), "{:?}", g.errors);
        assert_eq!(g.tokens.len(), TOKENS.len(), "still shows a complete, validated theme");
    }

    #[test]
    fn schema_matches_server_on_alpha_and_length() {
        let sch = schema();
        let pat = |g: &str, k: &str| regex::Regex::new(sch["properties"][g]["properties"][k]["pattern"].as_str().unwrap()).unwrap();
        for (g, k, kind, _) in TOKENS {
            if *kind != "color" {
                continue;
            }
            for v in ["#123", "#123456", "#12345680", "#123456ff", "#12345"] {
                let server = user("s", &format!("[{g}]\n{k} = \"{v}\"\n"), &[]);
                let server_ok = !server.errors.iter().any(|e| e.starts_with(&format!("{g}.{k}:")) || e.starts_with(&format!("{g}.{k} =")));
                assert_eq!(pat(g, k).is_match(v), server_ok, "{g}.{k} = {v}: {:?}", server.errors);
            }
        }
        let max = sch["properties"]["font"]["properties"]["ui"]["maxLength"].as_u64().unwrap() as usize;
        let at = "가".repeat(max);
        assert!(font_list(&at).is_ok(), "200 Hangul characters (600 bytes) fit like the schema says");
        assert!(font_list(&format!("{at}가")).is_err());
    }
}
